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
    // same function; `rosetta3::abi::table` already folds the suffix away.
    ("stat", STAT),
    ("fstat", STAT),
    ("lstat", STAT),
    // Kept listed although `crate::stat` now answers `fstatat`: the entry is what
    // stops the *host's* own being bound instead, and `reason` carves an
    // implemented name back out. It still has one live use - `deny_in` refuses
    // `_fstatat` in the host provider, so a `dlopen`ed host dylib importing it is
    // turned away rather than handed a glibc `struct stat` a guest would read at
    // Darwin's offsets.
    ("fstatat", STAT),
    ("statfs", STAT),
    ("fstatfs", STAT),
    // `statvfs` and `fstatvfs` are here for the same reason `statfs` and
    // `fstatfs` are, and the history is worth recording because it is not the
    // reason any other entry above is listed.
    //
    // They were **missing from this list**, and their absence cost a Chromium
    // run a smashed frame. The guest's `_statvfs` import was therefore not
    // claimed by a Darwin implementation *and* not denied to the host, so the
    // last provider — `NativeDylibProvider` — handed it **glibc's own
    // `statvfs`**, and glibc wrote a 112-byte `struct statvfs` into a buffer
    // the caller had sized for the guest's own **64-byte** one. Measured from a
    // core dump: `base::SysInfo::AmountOfFreeDiskSpace` at
    // `Framework+0x5d83b90` puts that buffer at `[rbp-0x70]`, and 112 bytes
    // from there ends at `[rbp+0x0]` — the two words it covers below `rbp` are
    // **exactly** that frame's saved `rbx` and `r14`, and the byte it does *not*
    // reach is the saved `rbp`. The function returned to exactly the right
    // place having replaced both callee-saved registers with zeros, and the
    // guest then stored through the null `rbx` and died at `Framework+0x65d1bac`
    // in `SimpleBackendImpl::InitCacheStructureOnDisk`.
    //
    // The control that pins the mechanism rather than merely suggesting it: of
    // the five callee-saved registers, **only `rbx` and `r14` were destroyed** —
    // `r12`, `r13` and `r15` all survived. 112 bytes of overrun predicts that
    // selection exactly, including the one register it stops short of; "a callee
    // was rude" does not predict anything.
    //
    // `crate::stat` now answers both, so like `fstatat` and `statfs` above the
    // entries stay: they are what stops the **host's** `statvfs` being bound
    // instead, and `reason` carves an implemented name back out. The size that
    // makes this a hazard is 64 and not the ~2 KB `struct statfs` is — see
    // `darwin::stat::Statvfs`, where all three measurements are recorded.
    ("statvfs", STAT),
    ("fstatvfs", STAT),
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
    ("regexec", REGEX),
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
    // is not the one Darwin describes. `rosetta3::signal` shims these.
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
    // Running a command would invoke the host shell against the host filesystem.
    // Neither is part of the Darwin process the guest was linked for.
    (
        "system",
        "would run the host shell against the host filesystem",
    ),
    (
        "ioctl",
        "BSD ioctl requests need translation before reaching the host",
    ),
    // VideoToolbox's *session* half is real video work rather than a feature
    // probe - a decode or encode path this runtime has no hardware, and no
    // software decoder, it could be built on. So these are refused by name
    // instead of being left unbound, because an unbound import is a jump
    // through a zero slot (PLAN.md §4) whereas a refusal says what it is.
    //
    // The two *probes* in the same family are answered for real - see
    // `crate::unavailable`, which returns false for
    // `VTIsHardwareDecodeSupported`, which is the truth on this host.
    (
        "VTDecompressionSessionCreate",
        "there is no VideoToolbox on this host, and no decoder it could be built on",
    ),
    (
        "VTCompressionSessionCreate",
        "there is no VideoToolbox on this host, and no encoder it could be built on",
    ),
    (
        "VTDecompressionSessionInvalidate",
        "as VTDecompressionSessionCreate: no session can exist to invalidate",
    ),
    (
        "VTCompressionSessionInvalidate",
        "as VTCompressionSessionCreate: no session can exist to invalidate",
    ),
    (
        "VTDecompressionSessionDecodeFrame",
        "as VTDecompressionSessionCreate",
    ),
    (
        "VTCompressionSessionEncodeFrame",
        "as VTCompressionSessionCreate",
    ),
    (
        "VTCompressionSessionCompleteFrames",
        "as VTCompressionSessionCreate",
    ),
    (
        "VTDecompressionSessionCanAcceptFormatDescription",
        "as VTDecompressionSessionCreate",
    ),
    ("VTSessionCopyProperty", "as VTDecompressionSessionCreate"),
    ("VTSessionSetProperty", "as VTDecompressionSessionCreate"),
    (
        "VTSessionCopySupportedPropertyDictionary",
        "as VTDecompressionSessionCreate",
    ),
    (
        "VTPixelTransferSessionCreate",
        "as VTDecompressionSessionCreate: pixel transfer needs VideoToolbox",
    ),
    (
        "VTPixelTransferSessionInvalidate",
        "as VTPixelTransferSessionCreate",
    ),
    (
        "VTPixelTransferSessionTransferImage",
        "as VTPixelTransferSessionCreate",
    ),
    (
        "VTCreateCGImageFromCVPixelBuffer",
        "as VTDecompressionSessionCreate: needs VideoToolbox to make the image",
    ),
];

#[cfg(target_os = "macos")]
pub const NAMES: &[(&str, &str)] = &[];

/// Families that must not reach the host.
#[cfg(not(target_os = "macos"))]
pub const PREFIXES: &[(&str, &str)] = &[
    // Mach is not an interface this host has. `rosetta3::shims::machthread` answers
    // the handful a guest actually needs.
    ("mach_", MACH),
    ("vm_", MACH),
    ("task_", MACH),
    ("thread_policy_", MACH),
    ("host_", MACH),
    ("semaphore_", MACH),
    ("bootstrap_", MACH),
    // dyld introspection, which `rosetta3::dyld` answers about the *guest's* images.
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
/// **This deliberately uses the non-reducing [`crate::implemented`], and the
/// reason is worth recording precisely because it is *not* a live bug.** A
/// prior review claimed this was a defect and it is not; do not "rediscover"
/// it as one, and do not revert this as noise.
///
/// The public [`crate::is_implemented`] wraps [`crate::implemented`] in a
/// `base_name` reduction, and every path that reaches here has *already*
/// reduced — `crate::refusal` and `crate::symbol` do `base_name(symbol)` first,
/// and `rosetta3::shims::host::host_or_refuse` does `c_name(symbol)` first. So
/// the old `is_implemented` here reduced **twice**.
///
/// That can only change an answer for a name carrying a **leading** underscore,
/// because `base_name` strips one leading underscore and nothing else. And
/// `implemented_here` only *matters* when the name would otherwise be caught by
/// the `NAMES` or `PREFIXES` scan below — every one of which begins with a
/// letter, so a name starting with `_` can never match one. The two conditions
/// are therefore mutually exclusive and the reduction is unreachable.
///
/// Measured, not argued: `mach_absolute_time`, `dispatch_once_f`,
/// `pthread_create`, `pthread_mutex_init`, `pthread_getspecific` and
/// `pthread_key_create` all return `None` from [`reason`] and all resolve
/// through [`crate::symbol`]; and `__mach_`, `__pthread_`, `__dispatch_`,
/// `_mach_` all return `None`.
///
/// It stays changed because the argument here is a *reduced* name, and
/// `is_implemented` is documented to take a Mach-O name — so this is the
/// function whose contract matches what it is handed. That is worth having
/// before someone edits one of the two call sites above.
///
/// The threading family needs two more lists than the others: `PASSED_THROUGH`
/// is what this crate has looked at and vouched for, and `SHIMMED_ABOVE` what it
/// leaves to the runtime above. Neither has a `symbol` entry.
fn implemented_here(name: &str) -> bool {
    // `name` arrives reduced, so this must not reduce again.
    crate::implemented(name).is_some()
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
        // A Mach call that reaches the kernel about another process, which is
        // the part of the family `crate::mach` still does not answer.
        //
        // This witness used to be `mach_vm_purgable_control`, and it moved for a
        // reason worth recording. `crate::mach::vm` now answers the whole
        // `mach_vm_*` map/region family, so that name was the last member of the
        // `mach_` prefix this crate declined - and it is no longer declined at
        // all: `crate::last_unavailable` now publishes it, because the Framework
        // imports it at `+0x81c4106` and the instruction *after* the call,
        // `+0x81c410b`, reads its out-parameter with no status test whatsoever.
        // Leaving it here would have meant leaving that import **unbound** - a
        // zero in its slot, and a jump through it on every Mach VM walk - which
        // is the one outcome in this project that is strictly worse than a
        // refusal. `divergent` denies a name; it cannot publish one.
        //
        // So the witness moves rather than disappears. `task_for_pid` is still
        // declined by the `task_` prefix (asserted independently, with
        // `task_threads`, in the test below), it carries the same `MACH` reason
        // (`PREFIXES` maps `mach_` and `task_` to the same string), and it is
        // still true: the kernel is not asked about another process here.
        assert_eq!(reason("task_for_pid"), Some(MACH));
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
            // The `mach_vm_*` map/region family, answered by `crate::mach::vm`
            // off the back of the verified `vm_map`/`vm_region_64` work. Named
            // here because the `mach_` prefix denies them wholesale, and a name
            // this crate answers for must be excused from its own family.
            "mach_vm_map",
            "mach_vm_remap",
            "mach_vm_region",
            "mach_vm_allocate",
            "mach_vm_deallocate",
            "mach_vm_read",
            "mach_vm_write",
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
            "dispatch_once_f",
            // The libdispatch this crate implements, which the `dispatch_` prefix
            // would otherwise deny along with the rest of the family.
            "dispatch_async",
            "dispatch_semaphore_wait",
            "dispatch_queue_create",
            // A Mach receive source over a real port, and the four names the
            // guest's `WaitableEventWatcher` and `base::apple` call sites need to
            // drive it. See `crate::dispatch::source`, which is also where the
            // source *types* this crate refuses are refused.
            "dispatch_source_create",
            "dispatch_source_set_event_handler",
            "dispatch_source_set_cancel_handler",
            "dispatch_source_cancel",
            "dispatch_resume",
            // The directory family, denied by name so the host's `struct
            // dirent` never becomes the guest's.
            "readdir",
            "readdir_r",
            "scandir",
        ] {
            assert_eq!(reason(name), None, "{name} is implemented and still denied");
        }
        // And the families around them are still shut: a member of one that this
        // crate has *not* implemented is refused rather than passed through. A
        // `dispatch_source_*` name outside the seven above is in exactly that
        // position - the rest of the source API is about data, timers and source
        // types this crate does not model, and none of it is answered.
        assert!(contains("dispatch_source_get_data"));
        assert!(contains("dispatch_source_set_timer"));
        assert!(contains("dispatch_source_testcancel"));
        // The half of Mach that is still refused: anything asking the kernel
        // about a task's memory, or about another process at all. `mach_vm_read`
        // is answered now, and so is `mach_vm_purgable_control` - `crate::mach::vm`
        // takes the first, and `crate::last_unavailable` publishes the second,
        // because the guest reads that one's out-parameter without testing the
        // status and an unbound slot there is a jump through a zero. The
        // `task_` prefix is therefore the witness now, and it is a *live* one:
        // `reason` returns `None` for anything `crate::is_implemented` answers
        // for, so this assertion would fail the moment one of them was published.
        assert!(contains("task_for_pid"));
        assert!(contains("task_threads"));
    }
}
