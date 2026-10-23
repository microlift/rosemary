// The reasons, shared by the entries that give them. Only the non-macOS lists have
// entries, so on macOS these name nothing - which is the same statement the empty
// lists below make.
#[cfg(not(target_os = "macos"))]
mod why {
    pub const STAT: &str = "struct stat is laid out differently";
    pub const PASSWD: &str = "struct passwd is laid out differently";
    pub const DIRENT: &str = "struct dirent is laid out differently";
    pub const SIGNAL: &str = "the signal numbers and sigaction layout differ";
    pub const TERMIOS: &str = "struct termios is laid out differently";
    pub const MACH: &str = "Mach is a Darwin kernel interface";
    pub const SYSCTL: &str = "reimplemented; see darwin::sysctl";
    pub const XATTR: &str =
        "the calls take two more arguments and the names live in a namespace here";
    pub const ERRNO: &str = "the errno numbers diverge above 34; see darwin::errno";
    pub const SOCKET: &str = "struct sockaddr and the AF_/MSG_ constants differ; see darwin::net";
    pub const OBJC: &str = "the Objective-C runtime is macOS-only";
    pub const REGEX: &str = "regex_t and regmatch_t are laid out differently, and REG_NEWLINE and REG_NOSUB have \
         each other's values; see darwin::regex";
    pub const PTHREAD: &str = "no Darwin-compatible implementation; see darwin::pthread for the family this \
         crate does implement";
}

#[cfg(not(target_os = "macos"))]
use why::*;

/// Exact names that must not reach the host. See [`PREFIXES`] for the families.
#[cfg(not(target_os = "macos"))]
pub const NAMES: &[(&str, &str)] = &[
    // `struct stat` differs in field order, in the width of `st_ino`, and in whether
    // the timespecs are `st_atimespec` or `st_atim`. The `$INODE64` spellings are the
    // same function; `rosemary::abi::table` already folds the suffix away.
    ("stat", STAT),
    ("fstat", STAT),
    ("lstat", STAT),
    ("fstatat", STAT),
    ("statfs", STAT),
    ("fstatfs", STAT),
    ("getfsstat", STAT),
    // `struct dirent`: Darwin has a `d_namlen` glibc has not, so `d_type` and
    // the name are two bytes later. `crate::dirent` answers all but the last;
    // the entries are what stop the host's own being bound instead. `opendir`
    // and `closedir` are listed although the handle passes through untouched -
    // one takes a path this crate case-folds, the other drops the entry
    // `readdir` published for the handle.
    ("opendir", DIRENT),
    ("readdir", DIRENT),
    ("readdir_r", DIRENT),
    ("closedir", DIRENT),
    ("scandir", DIRENT),
    ("alphasort", DIRENT),
    ("getdirentries", DIRENT),
    // The comparator's arguments are `(thunk, a, b)` on Darwin and `(a, b, thunk)`
    // under glibc. Passing this through calls the guest's comparator with its
    // arguments rotated, which sorts and does not crash.
    (
        "qsort_r",
        "the comparator takes its context argument in a different position",
    ),
    // The other two callback-taking searchers of the C library, and neither is a
    // comparator problem: what diverges is the structure the call fills in.
    // Darwin's `glob_t` is `{ gl_pathc, gl_matchc, gl_offs, gl_flags, gl_pathv }`
    // and glibc's is `{ gl_pathc, gl_pathv, gl_offs, gl_flags }`, so a guest
    // reading the results at Darwin's offset 32 reads glibc's `gl_closedir`.
    // Kept listed although `crate::glob` now answers both: the entry is what
    // stops the host's own from being bound instead, and `reason` carves an
    // implemented name back out. `globfree` matters as much as `glob` - it is
    // handed the same structure and frees what it reads out of it.
    (
        "glob",
        "glob_t is laid out differently - gl_pathv is the second field here and the fifth there",
    ),
    (
        "globfree",
        "as `glob`: it frees the pointer it reads at gl_pathv",
    ),
    // `regmatch_t` holds two `regoff_t`, which is sixty-four bits on Darwin and
    // `int` here, and a host `regcomp` writes sixty-four bytes into the
    // thirty-two a guest declared. Kept listed although `crate::regex` now
    // answers all four: the entry is what stops the host's own being bound
    // instead, and `reason` carves an implemented name back out.
    ("regcomp", REGEX),
    ("regncomp", REGEX),
    ("regexec", REGEX),
    ("regnexec", REGEX),
    ("regfree", REGEX),
    ("regerror", REGEX),
    // `FTSENT::fts_statp` is a `struct stat *`, which is the [`STAT`] divergence
    // one pointer removed - the walk would hand the guest this host's.
    ("fts_open", STAT),
    // Darwin's is the POSIX `int` form; glibc's returns `char *` and may not touch
    // the caller's buffer at all. Kept listed although `crate::string` now answers
    // it: the entry is what stops the host's own from being bound instead, and
    // `reason` carves an implemented name back out.
    (
        "strerror_r",
        "glibc returns a pointer rather than filling the caller's buffer",
    ),
    // `struct sigaction` differs in layout and the `SA_*` differ in value; a guest
    // handler installed through the host's would be entered with a `ucontext_t` that
    // is not the one Darwin describes. `rosemary::signal` shims these.
    ("sigaction", SIGNAL),
    ("sigaltstack", SIGNAL),
    ("sigprocmask", SIGNAL),
    ("sigsuspend", SIGNAL),
    // Darwin's `SIGINFO`, `SIGEMT` and `SIGBUS` are not the host's numbers.
    ("signal", SIGNAL),
    ("raise", SIGNAL),
    ("kill", SIGNAL),
    ("sigemptyset", SIGNAL),
    ("sigfillset", SIGNAL),
    ("sigaddset", SIGNAL),
    ("sigdelset", SIGNAL),
    ("sigismember", SIGNAL),
    ("sigpending", SIGNAL),
    ("killpg", SIGNAL),
    // The number names a different signal on each host, so the *name* it prints
    // is wrong rather than missing - `strerror`'s trap, one table over.
    ("strsignal", SIGNAL),
    ("psignal", SIGNAL),
    // `RLIMIT_*` are not the same numbers, and `RLIM_INFINITY` is a different
    // sentinel. `crate::rlimit` answers both; the entries are what stop the
    // host's own being bound instead.
    ("getrlimit", "the RLIMIT_ resource numbers differ"),
    ("setrlimit", "the RLIMIT_ resource numbers differ"),
    // `fd_set` is the same 128 bytes on both and the *timeout* is not; `crate::select`
    // answers both calls, and these entries are what stop the host's own being bound.
    ("select", "struct timeval is laid out differently"),
    ("pselect", "the sigset_t is Darwin's, and 32 bits wide"),
    // Listed although `crate::poll` now answers it, for the reason `strerror_r`
    // is: the entry is what stops the host's own from being bound instead.
    ("poll", "the POLL* event bits differ"),
    // `crate::fcntl` answers it. The commands agree only as far as `F_SETFL`, and
    // the flag word that command takes is an `open` flag word, which agrees on
    // nothing past the access mode.
    ("fcntl", "the fcntl commands and the O_ status flags differ"),
    ("open", "the O_ flags differ"),
    ("openat", "the O_ flags differ"),
    // Their ABI agrees, but their effect on the emulated per-socket
    // `SO_NOSIGPIPE` state must be observed. See `crate::net::descriptor`.
    ("close", "must discard emulated per-descriptor socket state"),
    ("dup", "must inherit emulated per-descriptor socket state"),
    ("dup2", "must replace emulated per-descriptor socket state"),
    // Darwin's `struct passwd` has BSD's `pw_change` and `pw_class` in the middle,
    // so `pw_dir` is at offset 48 - past the end of this host's whole structure.
    // `crate::passwd` answers the two lookups; the reentrant pair stay denied.
    ("getpwuid", PASSWD),
    ("getpwnam", PASSWD),
    ("getpwuid_r", PASSWD),
    ("getpwnam_r", PASSWD),
    // `struct termios` field order, and the `c_cc` indices.
    ("tcgetattr", TERMIOS),
    ("tcsetattr", TERMIOS),
    ("openpty", TERMIOS),
    // The socket API, every layer of which disagrees; `crate::net` answers all of
    // these, and the entries are what stop the host's own being bound instead.
    // `listen`, `shutdown` and the `inet_addr` family are absent on purpose: they
    // carry no platform constant.
    ("socket", SOCKET),
    ("socketpair", SOCKET),
    ("bind", SOCKET),
    ("connect", SOCKET),
    ("accept", SOCKET),
    ("getsockname", SOCKET),
    ("getpeername", SOCKET),
    ("send", SOCKET),
    ("recv", SOCKET),
    ("sendto", SOCKET),
    ("recvfrom", SOCKET),
    ("sendmsg", SOCKET),
    ("recvmsg", SOCKET),
    ("inet_pton", SOCKET),
    ("inet_ntop", SOCKET),
    ("connectx", SOCKET),
    // Not socket calls, and here for what they do to a socket: `SO_NOSIGPIPE` is a
    // property of the descriptor on Darwin and of the call here, and neither takes
    // flags. See `crate::net::socket`.
    (
        "write",
        "SO_NOSIGPIPE is per-descriptor there and per-call here",
    ),
    (
        "writev",
        "SO_NOSIGPIPE is per-descriptor there and per-call here",
    ),
    (
        "sendfile",
        "Darwin takes six arguments and reports the byte count through an off_t pointer",
    ),
    // `struct hostent` matches; `h_addrtype` inside it is an `AF_*`, and Darwin's
    // `AF_INET6` is this host's `AF_ROUTE`.
    ("gethostbyname", SOCKET),
    ("gethostbyname2", SOCKET),
    ("gethostbyaddr", SOCKET),
    (
        "setsockopt",
        "SOL_SOCKET and every SO_ name are different numbers",
    ),
    (
        "getsockopt",
        "SOL_SOCKET and every SO_ name are different numbers",
    ),
    ("getifaddrs", SOCKET),
    ("freeifaddrs", SOCKET),
    // Darwin's `struct addrinfo` orders `ai_addr` and `ai_canonname` the other way,
    // and its `AI_*`/`NI_*` are different bits.
    ("getaddrinfo", "struct addrinfo is laid out differently"),
    ("freeaddrinfo", "struct addrinfo is laid out differently"),
    (
        "gai_strerror",
        "the EAI_ codes differ, in value and in sign",
    ),
    ("getnameinfo", "the NI_ flags differ"),
    // BSD and glibc assign different integer values to locale categories and
    // to the category-mask bits. `crate::xlocale` translates these two; the
    // entries stop a library-specific lookup from finding glibc's versions.
    ("setlocale", "the LC_* category values differ"),
    ("newlocale", "the LC_*_MASK values differ"),
    ("duplocale", "a null locale has different semantics"),
    ("freelocale", "Darwin returns int and glibc returns void"),
    // These are still unsupported: `nl_item` is another platform-specific
    // integer namespace, and passing Darwin's value to glibc asks a different
    // question (or indexes no item at all).
    ("nl_langinfo", "the nl_item values differ"),
    ("nl_langinfo_l", "the nl_item values differ"),
    ("dladdr", "Dl_info is laid out differently"),
    // The clock family is reimplemented; see darwin::clock for why the
    // `clockid_t` cannot be passed through.
    (
        "clock_settime",
        "the clockid_t values differ; see darwin::clock",
    ),
    ("sysctl", SYSCTL),
    ("sysctlbyname", SYSCTL),
    ("sysctlnametomib", SYSCTL),
    // `struct utsname`'s fields are 256 bytes on Darwin and 65 under glibc, so the
    // buffer the guest passes is not the size the host would fill.
    ("uname", "struct utsname has different field widths"),
    // Two extra arguments on Darwin, and a flat namespace where this host has a
    // partitioned one. `crate::xattr` answers all four.
    ("getxattr", XATTR),
    ("setxattr", XATTR),
    ("removexattr", XATTR),
    ("listxattr", XATTR),
    // The message a number names is not the same message: the `errno` tables
    // diverge above 34. `crate::string` answers both.
    ("strerror", ERRNO),
    ("perror", ERRNO),
    (
        "ioctl",
        "BSD ioctl requests need translation before reaching the host",
    ),
];

#[cfg(target_os = "macos")]
pub const NAMES: &[(&str, &str)] = &[];

/// Families that must not reach the host.
#[cfg(not(target_os = "macos"))]
pub const PREFIXES: &[(&str, &str)] = &[
    // Mach is not an interface this host has. `rosemary::shims::machthread` answers
    // the handful a guest actually needs.
    ("mach_", MACH),
    ("vm_", MACH),
    ("task_", MACH),
    ("thread_policy_", MACH),
    ("host_", MACH),
    ("semaphore_", MACH),
    ("bootstrap_", MACH),
    // dyld introspection, which `rosemary::dyld` answers about the *guest's* images.
    ("dyld_", "the host has no dyld"),
    ("NSGet", "a Darwin process-introspection interface"),
    // The Objective-C runtime, libdispatch, and the frameworks above them. Not
    // stubbed: a guest that imports one is refused at the import rather than half an
    // hour into its run.
    ("objc_", OBJC),
    ("class_", OBJC),
    ("sel_", OBJC),
    ("protocol_", OBJC),
    ("dispatch_", "libdispatch is macOS-only"),
    ("CF", "CoreFoundation is macOS-only"),
    ("NS", "Foundation is macOS-only"),
    ("CG", "CoreGraphics is macOS-only"),
    ("CM", "CoreMedia is macOS-only"),
    ("IO", "IOKit is macOS-only"),
    ("Audio", "CoreAudio is macOS-only"),
    // The whole threading family, minus what `crate::pthread` implements or vouches
    // for - see [`reason`], which lets those through. Threading is the part of the C
    // library where a wrong constant does not fail but deadlocks, so a name nobody
    // has thought about is refused rather than passed through on the chance that it
    // agrees.
    ("pthread_", PTHREAD),
    ("os_unfair_lock", "a Darwin locking primitive"),
    ("os_log", "a Darwin logging interface"),
];

#[cfg(target_os = "macos")]
pub const PREFIXES: &[(&str, &str)] = &[];

pub fn reason(name: &str) -> Option<&'static str> {
    // A name this crate implements is not divergent, whatever family it is in.
    // Several of the prefixes below are denied wholesale precisely so that the
    // handful within them this crate does answer for can be carved back out
    // here: `mach_absolute_time` out of Mach, `dispatch_once_f` out of
    // libdispatch, and most of `pthread_`.
    if implemented_here(name) {
        return None;
    }
    if let Some((_, why)) = NAMES.iter().find(|(n, _)| *n == name) {
        return Some(why);
    }
    PREFIXES
        .iter()
        .find(|(p, _)| name.starts_with(p))
        .map(|(_, why)| *why)
}

/// Whether `crate` answers for `name` itself.
///
/// The threading family needs two more lists than the others: `PASSED_THROUGH`
/// is what this crate has looked at and vouched for, and `SHIMMED_ABOVE` what it
/// leaves to the runtime above. Neither has a `symbol` entry.
fn implemented_here(name: &str) -> bool {
    crate::is_implemented(name)
}

pub fn contains(name: &str) -> bool {
    reason(name).is_some()
}

/// Apply this list to a host resolver, so a denied name never becomes an address.
pub fn deny_in(provider: &mut machish::resolve::NativeDylibProvider) {
    for (name, _) in NAMES {
        // Both spellings: the resolver is asked with the Mach-O underscore.
        provider.deny(name);
        provider.deny(&format!("_{name}"));
    }
    for (prefix, _) in PREFIXES {
        provider.deny_prefix(prefix);
        provider.deny_prefix(&format!("_{prefix}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn a_mac_host_denies_nothing() {
        assert!(NAMES.is_empty() && PREFIXES.is_empty());
        assert!(!contains("readdir"));
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_divergence_is_named_rather_than_passed_through() {
        // The half of the directory family nothing has implemented. `readdir`
        // itself was here until `crate::dirent` answered it, and is asserted
        // the other way round below.
        assert_eq!(reason("getdirentries"), Some(DIRENT));
        // A Mach call that maps another task's memory into this one, which
        // `crate::mach` does not answer.
        assert_eq!(reason("mach_vm_remap"), Some(MACH));
        assert_eq!(
            reason("strlen"),
            None,
            "a genuine passthrough is not denied"
        );
    }

    /// The families denied by prefix have holes in them, and the holes are the
    /// point: a prefix is denied wholesale so that the few names this crate does
    /// answer for can be excused one at a time.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_name_this_crate_implements_is_not_denied_by_its_family() {
        for name in [
            "mach_absolute_time",
            "mach_timebase_info",
            // The synthetic port namespace; see [`crate::mach`]. The `mach_`,
            // `task_`, `host_`, `semaphore_` and `bootstrap_` prefixes are all
            // denied wholesale so that exactly this list can be carved back out.
            "mach_task_self",
            "mach_port_allocate",
            "mach_msg",
            "task_set_exception_ports",
            "host_get_exception_ports",
            "semaphore_wait",
            "bootstrap_check_in",
            // Another task's memory, which it reads and writes for the caller
            // through the process server.
            "mach_vm_read",
            "mach_vm_write",
            "mach_vm_protect",
            "dispatch_once_f",
            // The libdispatch this crate implements, which the `dispatch_` prefix
            // would otherwise deny along with the rest of the family.
            "dispatch_async",
            "dispatch_semaphore_wait",
            "dispatch_queue_create",
            // The directory family, denied by name so the host's `struct
            // dirent` never becomes the guest's.
            "readdir",
            "readdir_r",
            "scandir",
        ] {
            assert_eq!(reason(name), None, "{name} is implemented and still denied");
        }
        // And the families around them are still shut: a member of one that this
        // crate has *not* implemented is refused rather than passed through.
        assert!(contains("dispatch_io_create"));
        // The half of Mach that is still refused: mapping another task's
        // memory, and ending it.
        assert!(contains("mach_vm_remap"));
        assert!(contains("task_terminate"));
        for name in ["task_for_pid", "task_threads", "pid_for_task"] {
            assert!(!contains(name), "{name} is answered by the process server");
        }
    }
}
