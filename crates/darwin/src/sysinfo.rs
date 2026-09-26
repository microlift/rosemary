use crate::sysctl::{
    CTL_HW, CTL_KERN, HW_MACHINE, KERN_HOSTNAME, KERN_OSRELEASE, KERN_OSTYPE, KERN_VERSION,
};

#[cfg(not(target_os = "macos"))]
#[repr(C)]
struct Rusage {
    utime: [i64; 2],
    stime: [i64; 2],
    counters: [i64; 14],
}

/// `getrusage` converts Darwin's 32-bit `tv_usec` fields and byte-valued
/// `ru_maxrss` instead of exposing Linux's layout and KiB count.
#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn getrusage(who: i32, out: *mut Rusage) -> i32 {
    if out.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let mut host = std::mem::MaybeUninit::<libc::rusage>::uninit();
    if unsafe { libc::getrusage(who, host.as_mut_ptr()) } != 0 {
        return -1;
    }
    let host = unsafe { host.assume_init() };
    let counters = [
        host.ru_maxrss.saturating_mul(1024),
        host.ru_ixrss,
        host.ru_idrss,
        host.ru_isrss,
        host.ru_minflt,
        host.ru_majflt,
        host.ru_nswap,
        host.ru_inblock,
        host.ru_oublock,
        host.ru_msgsnd,
        host.ru_msgrcv,
        host.ru_nsignals,
        host.ru_nvcsw,
        host.ru_nivcsw,
    ];
    // The array is explicitly Darwin's fourteen `long` counters.
    unsafe {
        out.write(Rusage {
            utime: [
                host.ru_utime.tv_sec,
                i64::from(host.ru_utime.tv_usec as i32),
            ],
            stime: [
                host.ru_stime.tv_sec,
                i64::from(host.ru_stime.tv_usec as i32),
            ],
            counters,
        })
    };
    0
}

#[cfg(not(target_os = "macos"))]
const PATHCONF: &[(i32, i32)] = &[
    (1, libc::_PC_LINK_MAX),
    (2, libc::_PC_MAX_CANON),
    (3, libc::_PC_MAX_INPUT),
    (4, libc::_PC_NAME_MAX),
    (5, libc::_PC_PATH_MAX),
    (6, libc::_PC_PIPE_BUF),
    (7, libc::_PC_CHOWN_RESTRICTED),
    (8, libc::_PC_NO_TRUNC),
    (9, libc::_PC_VDISABLE),
    (10, libc::_PC_SYMLINK_MAX),
];
#[cfg(not(target_os = "macos"))]
fn pathconf_name(name: i32) -> Option<i32> {
    PATHCONF.iter().find(|(d, _)| *d == name).map(|(_, h)| *h)
}
#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn pathconf(path: *const libc::c_char, name: i32) -> i64 {
    let Some(name) = pathconf_name(name) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    // The path is the guest's; see [`crate::fs`] for why it is translated.
    crate::stat::with_fallback(path, |path| {
        let value = unsafe { libc::pathconf(path, name) as i64 };
        (value, value >= 0)
    })
}
#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn fpathconf(fd: i32, name: i32) -> i64 {
    match pathconf_name(name) {
        Some(name) => unsafe { libc::fpathconf(fd, name) as i64 },
        None => {
            crate::errno::set(libc::EINVAL);
            -1
        }
    }
}
#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn confstr(name: i32, out: *mut libc::c_char, len: usize) -> usize {
    if name == 1 {
        unsafe { libc::confstr(libc::_CS_PATH, out, len) }
    } else {
        crate::errno::set(libc::EINVAL);
        0
    }
}

/// `_SYS_NAMELEN`, the width of every field in Darwin's `struct utsname`.
///
/// The reason `uname` cannot be passed through on Linux: glibc's fields are 65 bytes
/// and musl's are 65 too, so the host would write five short strings into a 1280-byte
/// buffer at the wrong five offsets.
pub const SYS_NAMELEN: usize = 256;

/// Darwin's `struct utsname`. Five fixed-width strings, nothing else.
#[repr(C)]
pub struct Utsname {
    pub sysname: [u8; SYS_NAMELEN],
    pub nodename: [u8; SYS_NAMELEN],
    pub release: [u8; SYS_NAMELEN],
    pub version: [u8; SYS_NAMELEN],
    pub machine: [u8; SYS_NAMELEN],
}

/// `int uname(struct utsname *)`.
///
/// Filled from this crate's own [`crate::sysctl`] rather than from the host, which is
/// what keeps `uname -m` and `sysctl hw.machine` agreeing: both are questions about
/// the guest, and answering them from two places is how they come to disagree.
///
/// # Safety
///
/// `out` points at a writable Darwin `struct utsname`.
unsafe extern "C" fn uname(out: *mut Utsname) -> i32 {
    if out.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    let fields = [
        (0usize, ask(&[CTL_KERN, KERN_OSTYPE])),
        (1, ask(&[CTL_KERN, KERN_HOSTNAME])),
        (2, ask(&[CTL_KERN, KERN_OSRELEASE])),
        (3, ask(&[CTL_KERN, KERN_VERSION])),
        (4, ask(&[CTL_HW, HW_MACHINE])),
    ];
    for (index, value) in fields {
        // SAFETY: `out` is a writable `utsname`, whose five fields are contiguous
        // arrays of `SYS_NAMELEN` bytes each; `index` is one of the five.
        let field = unsafe { (out as *mut u8).add(index * SYS_NAMELEN) };
        let bytes = value.as_bytes();
        let n = bytes.len().min(SYS_NAMELEN - 1);
        // SAFETY: `field` is `SYS_NAMELEN` writable bytes and `n` is at most one less.
        unsafe {
            std::ptr::write_bytes(field, 0, SYS_NAMELEN);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), field, n);
        }
    }
    0
}

/// One `sysctl` string, or the empty string if nothing answers.
///
/// Goes through the exported `sysctl` rather than around it, so a guest that reads a
/// key directly and a guest that calls `uname` cannot be told different things.
fn ask(mib: &[i32]) -> String {
    let Some(sysctl) = crate::sysctl::symbol("sysctl") else {
        return String::new();
    };
    // SAFETY: `sysctl::symbol` returns this crate's own `sysctl`, whose prototype is
    // the one named here.
    let f: unsafe extern "C" fn(
        *const libc::c_int,
        libc::c_uint,
        *mut libc::c_void,
        *mut libc::size_t,
        *mut libc::c_void,
        libc::size_t,
    ) -> i32 = unsafe { std::mem::transmute(sysctl as usize as *const ()) };

    let mut buf = [0u8; SYS_NAMELEN];
    let mut len: libc::size_t = buf.len();
    // SAFETY: `mib` is live for its own length and `buf` for `len` bytes.
    let rc = unsafe {
        f(
            mib.as_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        return String::new();
    }
    let end = buf[..len].iter().position(|b| *b == 0).unwrap_or(len);
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Darwin's `_SC_*` names, which are numbered nothing like glibc's.
///
/// Both platforms hand `sysconf` a small integer and neither publishes the same
/// integer for the same question. The one that matters most is `_SC_PAGESIZE`:
/// Darwin calls it 29, glibc calls 29 `_SC_VERSION`, and a guest asking for the
/// page size is answered `200809`. mimalloc rounds its page-map reservation up
/// to that "page size", asks for a mapping of the result, gets nothing, and the
/// program dies in a static initializer with an uncaught `std::bad_alloc` - with
/// no mention of `sysconf` anywhere in the wreckage.
///
/// From macOS's `<sys/unistd.h>`. Only the block this list covers contiguously,
/// 1 through 52, plus the handful above it that programs actually ask for; a
/// name outside it is `EINVAL` rather than a guess, because a guess here is the
/// bug being fixed.
#[cfg(not(target_os = "macos"))]
mod sc {
    pub const ARG_MAX: i32 = 1;
    pub const CHILD_MAX: i32 = 2;
    pub const CLK_TCK: i32 = 3;
    pub const NGROUPS_MAX: i32 = 4;
    pub const OPEN_MAX: i32 = 5;
    pub const JOB_CONTROL: i32 = 6;
    pub const SAVED_IDS: i32 = 7;
    pub const VERSION: i32 = 8;
    pub const BC_BASE_MAX: i32 = 9;
    pub const BC_DIM_MAX: i32 = 10;
    pub const BC_SCALE_MAX: i32 = 11;
    pub const BC_STRING_MAX: i32 = 12;
    pub const COLL_WEIGHTS_MAX: i32 = 13;
    pub const EXPR_NEST_MAX: i32 = 14;
    pub const LINE_MAX: i32 = 15;
    pub const RE_DUP_MAX: i32 = 16;
    pub const TWO_VERSION: i32 = 17;
    pub const TWO_C_BIND: i32 = 18;
    pub const TWO_C_DEV: i32 = 19;
    pub const TWO_CHAR_TERM: i32 = 20;
    pub const TWO_FORT_DEV: i32 = 21;
    pub const TWO_FORT_RUN: i32 = 22;
    pub const TWO_LOCALEDEF: i32 = 23;
    pub const TWO_SW_DEV: i32 = 24;
    pub const TWO_UPE: i32 = 25;
    pub const STREAM_MAX: i32 = 26;
    pub const TZNAME_MAX: i32 = 27;
    pub const ASYNCHRONOUS_IO: i32 = 28;
    pub const PAGESIZE: i32 = 29;
    pub const MEMLOCK: i32 = 30;
    pub const MEMLOCK_RANGE: i32 = 31;
    pub const MEMORY_PROTECTION: i32 = 32;
    pub const MESSAGE_PASSING: i32 = 33;
    pub const PRIORITIZED_IO: i32 = 34;
    pub const PRIORITY_SCHEDULING: i32 = 35;
    pub const REALTIME_SIGNALS: i32 = 36;
    pub const SEMAPHORES: i32 = 37;
    pub const FSYNC: i32 = 38;
    pub const SHARED_MEMORY_OBJECTS: i32 = 39;
    pub const SYNCHRONIZED_IO: i32 = 40;
    pub const AIO_LISTIO_MAX: i32 = 42;
    pub const AIO_MAX: i32 = 43;
    pub const AIO_PRIO_DELTA_MAX: i32 = 44;
    pub const DELAYTIMER_MAX: i32 = 45;
    pub const MQ_OPEN_MAX: i32 = 46;
    pub const MAPPED_FILES: i32 = 47;
    pub const RTSIG_MAX: i32 = 48;
    pub const SEM_NSEMS_MAX: i32 = 49;
    pub const SEM_VALUE_MAX: i32 = 50;
    pub const SIGQUEUE_MAX: i32 = 51;
    pub const TIMER_MAX: i32 = 52;
    pub const IOV_MAX: i32 = 56;
    pub const NPROCESSORS_CONF: i32 = 57;
    pub const NPROCESSORS_ONLN: i32 = 58;
    pub const GETGR_R_SIZE_MAX: i32 = 70;
    pub const GETPW_R_SIZE_MAX: i32 = 71;
    pub const HOST_NAME_MAX: i32 = 72;
    pub const LOGIN_NAME_MAX: i32 = 73;
    pub const THREAD_DESTRUCTOR_ITERATIONS: i32 = 85;
    pub const THREAD_KEYS_MAX: i32 = 86;
    pub const THREAD_STACK_MIN: i32 = 93;
    pub const THREAD_THREADS_MAX: i32 = 94;
    pub const TTY_NAME_MAX: i32 = 101;
    pub const ATEXIT_MAX: i32 = 107;
    pub const PHYS_PAGES: i32 = 200;
}

/// The host's `_SC_*` for a guest's Darwin one, or `None` for a name this
/// mapping does not carry.
#[cfg(not(target_os = "macos"))]
fn host_confname(name: i32) -> Option<i32> {
    Some(match name {
        sc::ARG_MAX => libc::_SC_ARG_MAX,
        sc::CHILD_MAX => libc::_SC_CHILD_MAX,
        sc::CLK_TCK => libc::_SC_CLK_TCK,
        sc::NGROUPS_MAX => libc::_SC_NGROUPS_MAX,
        sc::OPEN_MAX => libc::_SC_OPEN_MAX,
        sc::JOB_CONTROL => libc::_SC_JOB_CONTROL,
        sc::SAVED_IDS => libc::_SC_SAVED_IDS,
        sc::VERSION => libc::_SC_VERSION,
        sc::BC_BASE_MAX => libc::_SC_BC_BASE_MAX,
        sc::BC_DIM_MAX => libc::_SC_BC_DIM_MAX,
        sc::BC_SCALE_MAX => libc::_SC_BC_SCALE_MAX,
        sc::BC_STRING_MAX => libc::_SC_BC_STRING_MAX,
        sc::COLL_WEIGHTS_MAX => libc::_SC_COLL_WEIGHTS_MAX,
        sc::EXPR_NEST_MAX => libc::_SC_EXPR_NEST_MAX,
        sc::LINE_MAX => libc::_SC_LINE_MAX,
        sc::RE_DUP_MAX => libc::_SC_RE_DUP_MAX,
        sc::TWO_VERSION => libc::_SC_2_VERSION,
        sc::TWO_C_BIND => libc::_SC_2_C_BIND,
        sc::TWO_C_DEV => libc::_SC_2_C_DEV,
        sc::TWO_CHAR_TERM => libc::_SC_2_CHAR_TERM,
        sc::TWO_FORT_DEV => libc::_SC_2_FORT_DEV,
        sc::TWO_FORT_RUN => libc::_SC_2_FORT_RUN,
        sc::TWO_LOCALEDEF => libc::_SC_2_LOCALEDEF,
        sc::TWO_SW_DEV => libc::_SC_2_SW_DEV,
        sc::TWO_UPE => libc::_SC_2_UPE,
        sc::STREAM_MAX => libc::_SC_STREAM_MAX,
        sc::TZNAME_MAX => libc::_SC_TZNAME_MAX,
        sc::ASYNCHRONOUS_IO => libc::_SC_ASYNCHRONOUS_IO,
        sc::PAGESIZE => libc::_SC_PAGESIZE,
        sc::MEMLOCK => libc::_SC_MEMLOCK,
        sc::MEMLOCK_RANGE => libc::_SC_MEMLOCK_RANGE,
        sc::MEMORY_PROTECTION => libc::_SC_MEMORY_PROTECTION,
        sc::MESSAGE_PASSING => libc::_SC_MESSAGE_PASSING,
        sc::PRIORITIZED_IO => libc::_SC_PRIORITIZED_IO,
        sc::PRIORITY_SCHEDULING => libc::_SC_PRIORITY_SCHEDULING,
        sc::REALTIME_SIGNALS => libc::_SC_REALTIME_SIGNALS,
        sc::SEMAPHORES => libc::_SC_SEMAPHORES,
        sc::FSYNC => libc::_SC_FSYNC,
        sc::SHARED_MEMORY_OBJECTS => libc::_SC_SHARED_MEMORY_OBJECTS,
        sc::SYNCHRONIZED_IO => libc::_SC_SYNCHRONIZED_IO,
        sc::AIO_LISTIO_MAX => libc::_SC_AIO_LISTIO_MAX,
        sc::AIO_MAX => libc::_SC_AIO_MAX,
        sc::AIO_PRIO_DELTA_MAX => libc::_SC_AIO_PRIO_DELTA_MAX,
        sc::DELAYTIMER_MAX => libc::_SC_DELAYTIMER_MAX,
        sc::MQ_OPEN_MAX => libc::_SC_MQ_OPEN_MAX,
        sc::MAPPED_FILES => libc::_SC_MAPPED_FILES,
        sc::RTSIG_MAX => libc::_SC_RTSIG_MAX,
        sc::SEM_NSEMS_MAX => libc::_SC_SEM_NSEMS_MAX,
        sc::SEM_VALUE_MAX => libc::_SC_SEM_VALUE_MAX,
        sc::SIGQUEUE_MAX => libc::_SC_SIGQUEUE_MAX,
        sc::TIMER_MAX => libc::_SC_TIMER_MAX,
        sc::IOV_MAX => libc::_SC_IOV_MAX,
        sc::NPROCESSORS_CONF => libc::_SC_NPROCESSORS_CONF,
        sc::NPROCESSORS_ONLN => libc::_SC_NPROCESSORS_ONLN,
        sc::GETGR_R_SIZE_MAX => libc::_SC_GETGR_R_SIZE_MAX,
        sc::GETPW_R_SIZE_MAX => libc::_SC_GETPW_R_SIZE_MAX,
        sc::HOST_NAME_MAX => libc::_SC_HOST_NAME_MAX,
        sc::LOGIN_NAME_MAX => libc::_SC_LOGIN_NAME_MAX,
        sc::THREAD_DESTRUCTOR_ITERATIONS => libc::_SC_THREAD_DESTRUCTOR_ITERATIONS,
        sc::THREAD_KEYS_MAX => libc::_SC_THREAD_KEYS_MAX,
        sc::THREAD_STACK_MIN => libc::_SC_THREAD_STACK_MIN,
        sc::THREAD_THREADS_MAX => libc::_SC_THREAD_THREADS_MAX,
        sc::TTY_NAME_MAX => libc::_SC_TTY_NAME_MAX,
        sc::ATEXIT_MAX => libc::_SC_ATEXIT_MAX,
        sc::PHYS_PAGES => libc::_SC_PHYS_PAGES,
        _ => return None,
    })
}

/// `long sysconf(int name)`, with `name` translated out of Darwin's numbering.
///
/// Only the name is rewritten. The *value* is the host's own answer, because
/// every entry above is a question about the machine this is actually running
/// on: the page size has to be the real one or a mapping is the wrong size, and
/// the processor count has to be the real one or a thread pool is the wrong
/// shape.
#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn sysconf(name: i32) -> libc::c_long {
    let Some(host) = host_confname(name) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    // SAFETY: a name this host publishes; `sysconf` takes no pointer and reads a
    // static system parameter.
    unsafe { libc::sysconf(host) }
}

/// `int compat_mode(const char *command, const char *mode)`.
///
/// Darwin-only, and there is nothing on Linux to pass it through to. A command asks
/// which of two historical behaviours it should present; `uname` asks about
/// `"unix2003"` and prints its fields in a different order depending on the answer.
///
/// A modern macOS answers 1 for `unix2003` unless `COMMAND_MODE` says `legacy`, which
/// is the rule reproduced here - `COMMAND_MODE` being the environment variable the
/// real implementation reads.
///
/// The comparison is case-insensitive because the callers are: `/usr/bin/uname` asks
/// about `"Unix2003"` with a capital U, and an exact match against the lowercase
/// spelling answers 0 - which is a legacy `uname -a`, with a `-p` field real macOS
/// does not print.
unsafe extern "C" fn compat_mode(
    _command: *const libc::c_char,
    mode: *const libc::c_char,
) -> libc::c_int {
    if mode.is_null() {
        return 0;
    }
    // SAFETY: the caller states `mode` is NUL-terminated.
    let mode = unsafe { std::ffi::CStr::from_ptr(mode) };
    let legacy = std::env::var("COMMAND_MODE").is_ok_and(|m| m.eq_ignore_ascii_case("legacy"));
    let mode = mode.to_bytes();
    if mode.eq_ignore_ascii_case(b"unix2003") {
        return libc::c_int::from(!legacy);
    }
    libc::c_int::from(mode.eq_ignore_ascii_case(b"legacy") && legacy)
}

/// `OSErr Gestalt(OSType selector, SInt32 *response)`, the Carbon question a
/// program still asks about the OS version.
///
/// Only the version selectors. Gestalt has hundreds, and every other one is
/// about hardware this runtime is not - so an unknown selector is
/// `gestaltUndefSelectorErr`, which is what Apple's answers for a selector the
/// system does not implement, and a caller that checks gets told rather than
/// handed a number about some other machine.
///
/// The version is [`crate::sysctl::macos_version`]'s, so this and
/// `kern.osrelease` cannot disagree.
///
/// # Safety
///
/// `response` is null or a writable `SInt32 *`.
#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn gestalt(selector: u32, response: *mut i32) -> i16 {
    /// `paramErr`.
    const PARAM_ERROR: i16 = -50;
    /// `gestaltUndefSelectorErr`.
    const UNDEFINED_SELECTOR: i16 = -5551;

    if response.is_null() {
        return PARAM_ERROR;
    }
    let (major, minor, patch) = crate::sysctl::macos_version();
    let bcd = |n: i32| ((n / 10) << 4) | (n % 10);
    let value = match &selector.to_be_bytes() {
        b"sys1" => major,
        b"sys2" => minor,
        b"sys3" => patch,
        // `gestaltSystemVersion`, a BCD word. Apple caps it at 0x1039 for every
        // release after 10.3.9 precisely so that old code comparing it does not
        // read 11.0 as older than 10.9, and a runtime that reported the true
        // number would reintroduce the bug the cap exists for.
        b"sysv" => 0x1039,
        _ => return UNDEFINED_SELECTOR,
    };
    // SAFETY: the caller states `response` is writable.
    unsafe {
        *response = if selector.to_be_bytes() == *b"sysv" {
            value
        } else {
            bcd(value)
        }
    };
    0
}

// Not the same list on both hosts: `Gestalt` is CoreServices' and is passed
// through where there is one, and `sysconf`'s names are Darwin's already on a
// Mac.
/// Darwin's `NXArchInfo`.
#[repr(C)]
pub struct ArchInfo {
    name: *const std::ffi::c_char,
    cputype: i32,
    cpusubtype: i32,
    byteorder: u32,
    description: *const std::ffi::c_char,
}

// SAFETY: every pointer is to a `'static` string literal.
unsafe impl Sync for ArchInfo {}

/// The architecture the guest is running as: an x86-64 guest asks, and an
/// x86-64 Mac answers `x86_64`, `CPU_TYPE_X86_64` and `CPU_SUBTYPE_X86_64_ALL`.
static LOCAL_ARCH: ArchInfo = ArchInfo {
    name: c"x86_64".as_ptr(),
    cputype: 0x0100_0007,
    cpusubtype: 3,
    // `NX_LittleEndian`.
    byteorder: 1,
    description: c"Intel x86-64".as_ptr(),
};

/// `const NXArchInfo *NXGetLocalArchInfo(void)`
pub extern "C" fn nx_get_local_arch_info() -> *const ArchInfo {
    &LOCAL_ARCH
}

exports::exports! {
    functions {
        "uname" => uname,
        "NXGetLocalArchInfo" => nx_get_local_arch_info,
        "compat_mode" => compat_mode,
        #[cfg(not(target_os = "macos"))]
        "Gestalt" => gestalt,
        #[cfg(not(target_os = "macos"))]
        "sysconf" => sysconf,
        #[cfg(not(target_os = "macos"))]
        "getrusage" => getrusage,
        #[cfg(not(target_os = "macos"))]
        "pathconf" => pathconf,
        #[cfg(not(target_os = "macos"))]
        "fpathconf" => fpathconf,
        #[cfg(not(target_os = "macos"))]
        "confstr" => confstr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysctl::{GuestArch, lock_guest_arch};

    fn field(u: &Utsname, index: usize) -> String {
        // SAFETY: the five fields are contiguous `SYS_NAMELEN` arrays and `index` is
        // one of the five.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (u as *const Utsname as *const u8).add(index * SYS_NAMELEN),
                SYS_NAMELEN,
            )
        };
        let end = bytes.iter().position(|b| *b == 0).unwrap_or(SYS_NAMELEN);
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    }

    fn called() -> Utsname {
        // SAFETY: zero is a valid `utsname` - five empty strings.
        let mut u: Utsname = unsafe { std::mem::zeroed() };
        // SAFETY: `u` is a live writable `utsname`.
        assert_eq!(unsafe { uname(&mut u) }, 0);
        u
    }

    #[test]
    fn the_system_is_darwin_and_the_machine_is_the_guest_s() {
        let _held = lock_guest_arch(GuestArch::X86_64);
        let u = called();
        assert_eq!(field(&u, 0), "Darwin");
        assert_eq!(field(&u, 4), "x86_64");
        assert!(!field(&u, 2).is_empty(), "a release is reported");
    }

    /// `uname` and a direct `sysctl` are the same question, and must not be capable
    /// of different answers.
    #[test]
    fn uname_agrees_with_sysctl() {
        let _held = lock_guest_arch(GuestArch::Arm64);
        let u = called();
        assert_eq!(field(&u, 4), ask(&[CTL_HW, HW_MACHINE]));
    }

    /// Every field must be NUL-terminated within its own 256 bytes, because the guest
    /// reads each as a C string starting at a fixed offset.
    #[test]
    fn every_field_is_terminated_inside_its_own_width() {
        let u = called();
        // SAFETY: `u` is a live `utsname`, read as its own bytes.
        let bytes = unsafe {
            std::slice::from_raw_parts(&u as *const Utsname as *const u8, size_of::<Utsname>())
        };
        for i in 0..5 {
            let f = &bytes[i * SYS_NAMELEN..(i + 1) * SYS_NAMELEN];
            assert!(f.contains(&0), "field {i} runs into the next");
        }
    }

    #[test]
    fn the_struct_is_the_width_darwin_declares() {
        assert_eq!(size_of::<Utsname>(), 5 * 256);
    }

    /// The capitalised spelling is not a corner case - it is the one `/usr/bin/uname`
    /// actually passes, and answering 0 to it prints a field real macOS does not.
    #[test]
    fn unix2003_is_in_effect_whatever_case_it_is_asked_in() {
        let cmd = std::ffi::CString::new("bin/uname").unwrap();
        for spelling in ["unix2003", "Unix2003", "UNIX2003"] {
            let mode = std::ffi::CString::new(spelling).unwrap();
            // SAFETY: both arguments are NUL-terminated and outlive the call.
            assert_eq!(
                unsafe { compat_mode(cmd.as_ptr(), mode.as_ptr()) },
                1,
                "{spelling}"
            );
        }
        let legacy = std::ffi::CString::new("legacy").unwrap();
        unsafe {
            assert_eq!(compat_mode(cmd.as_ptr(), legacy.as_ptr()), 0);
            assert_eq!(compat_mode(cmd.as_ptr(), std::ptr::null()), 0);
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_page_size_is_a_page_size() {
        // SAFETY: `sysconf` takes an integer and reads a system parameter.
        let page = unsafe { sysconf(sc::PAGESIZE) };
        assert_eq!(page, machish::memory::host_page_size() as libc::c_long);
        assert!(
            page > 0 && (page as u64).is_power_of_two(),
            "{page} is not a page size"
        );
    }

    /// Darwin's numbering is not glibc's anywhere in the block, so a name that
    /// happens to collide must still come back with its own answer.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn each_name_is_translated_rather_than_passed_through() {
        // SAFETY: each is an integer argument to a parameter query.
        unsafe {
            assert_eq!(sysconf(sc::VERSION), libc::sysconf(libc::_SC_VERSION));
            assert_eq!(
                sysconf(sc::NPROCESSORS_ONLN),
                libc::sysconf(libc::_SC_NPROCESSORS_ONLN)
            );
            assert!(sysconf(sc::NPROCESSORS_ONLN) >= 1);
            assert_eq!(sysconf(sc::OPEN_MAX), libc::sysconf(libc::_SC_OPEN_MAX));
        }
    }

    /// A name this mapping does not carry is refused rather than guessed at,
    /// because guessing is what turned a page size into `200809`.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn an_unmapped_name_is_einval() {
        // SAFETY: an integer argument; 9999 names nothing on either side.
        assert_eq!(unsafe { sysconf(9999) }, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
    }
}
