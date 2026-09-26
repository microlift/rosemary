use std::sync::atomic::{AtomicU8, Ordering};

/// Top-level MIB identifiers.
pub const CTL_KERN: i32 = 1;
pub const CTL_HW: i32 = 6;
pub const CTL_USER: i32 = 8;

/// `CTL_KERN` second-level identifiers, for the keys answered here.
pub const KERN_OSTYPE: i32 = 1;
pub const KERN_OSRELEASE: i32 = 2;
pub const KERN_OSREV: i32 = 3;
pub const KERN_VERSION: i32 = 4;
pub const KERN_HOSTNAME: i32 = 10;
pub const KERN_ARGMAX: i32 = 8;
pub const KERN_OSVERSION: i32 = 65;

/// `CTL_USER` second-level identifiers.
pub const USER_CS_PATH: i32 = 1;

/// `CTL_HW` second-level identifiers.
pub const HW_MACHINE: i32 = 1;
pub const HW_MODEL: i32 = 2;
pub const HW_NCPU: i32 = 3;
pub const HW_BYTEORDER: i32 = 4;
pub const HW_PAGESIZE: i32 = 7;
pub const HW_MACHINE_ARCH: i32 = 12;
pub const HW_CACHELINE: i32 = 16;
pub const HW_MEMSIZE: i32 = 24;
pub const HW_AVAILCPU: i32 = 25;

/// The architecture the **guest** believes it is running on.
///
/// Set once when a program is loaded, because it is a property of the image rather
/// than of this host: the same runtime on the same Mac answers `x86_64` for an Intel
/// guest and `arm64` for a native one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuestArch {
    X86_64,
    Arm64,
}

impl GuestArch {
    /// `hw.machine`, which is what `uname -m` prints.
    fn machine(self) -> &'static str {
        match self {
            GuestArch::X86_64 => "x86_64",
            GuestArch::Arm64 => "arm64",
        }
    }

    /// `hw.machine_arch` and `uname -p`, which Darwin answers more coarsely.
    fn machine_arch(self) -> &'static str {
        match self {
            GuestArch::X86_64 => "i386",
            GuestArch::Arm64 => "arm",
        }
    }

    /// `cpu_type_t` as `<mach/machine.h>` numbers it: `CPU_ARCH_ABI64`
    /// (`0x0100_0000`) over the 32-bit type.
    pub fn cpu_type(self) -> i32 {
        match self {
            GuestArch::X86_64 => 0x0100_0007,
            GuestArch::Arm64 => 0x0100_000c,
        }
    }

    /// `CPU_SUBTYPE_X86_64_ALL` and `CPU_SUBTYPE_ARM64_ALL`.
    pub fn cpu_subtype(self) -> i32 {
        match self {
            GuestArch::X86_64 => 3,
            GuestArch::Arm64 => 0,
        }
    }
}

/// Zero until [`set_guest_arch`], which is how "nobody said" is told from "x86-64".
static GUEST_ARCH: AtomicU8 = AtomicU8::new(0);

/// Record which architecture the loaded program is.
pub fn set_guest_arch(arch: GuestArch) {
    GUEST_ARCH.store(
        match arch {
            GuestArch::X86_64 => 1,
            GuestArch::Arm64 => 2,
        },
        Ordering::Release,
    );
}

/// The guest's architecture, defaulting to this host's when no program said.
///
/// The default matters for the runtime's own start-up, which may ask before a program
/// is loaded; answering about the host is the answer that surprises nobody.
pub fn guest_arch() -> GuestArch {
    match GUEST_ARCH.load(Ordering::Acquire) {
        1 => GuestArch::X86_64,
        2 => GuestArch::Arm64,
        _ if cfg!(target_arch = "aarch64") => GuestArch::Arm64,
        _ => GuestArch::X86_64,
    }
}

/// One key's answer.
///
/// On macOS only [`Value::Str`] is ever built - every numeric key is forwarded to the
/// host's own `sysctl` - so the integer forms are dead there by construction.
#[cfg_attr(target_os = "macos", allow(dead_code))]
enum Value {
    Str(String),
    U32(u32),
    U64(u64),
}

impl Value {
    /// The bytes `sysctl` copies out, including a string's terminating NUL.
    fn bytes(&self) -> Vec<u8> {
        match self {
            Value::Str(s) => {
                let mut v = s.as_bytes().to_vec();
                v.push(0);
                v
            }
            Value::U32(n) => n.to_ne_bytes().to_vec(),
            Value::U64(n) => n.to_ne_bytes().to_vec(),
        }
    }
}

/// Keys whose answer is about the **guest**, and so is the same on either host.
///
/// Consulted before the host's own `sysctl`, which is the whole reason this module
/// intercepts on macOS at all.
fn guest_value(mib: &[i32]) -> Option<Value> {
    let arch = guest_arch();
    match mib {
        [CTL_HW, HW_MACHINE] => Some(Value::Str(arch.machine().to_string())),
        [CTL_HW, HW_MACHINE_ARCH] => Some(Value::Str(arch.machine_arch().to_string())),
        _ => None,
    }
}

/// Keys answered from this module's own table.
///
/// Only reached on a host with no Darwin `sysctl` behind it. Deliberately narrow: a
/// key nobody has needed yet is better as an `ENOENT` naming itself under
/// `ROSETTA3_TRACE_SYSCTL` than as a plausible invention.
#[cfg(not(target_os = "macos"))]
fn host_value(mib: &[i32]) -> Option<Value> {
    match mib {
        [CTL_KERN, KERN_OSTYPE] => Some(Value::Str("Darwin".to_string())),
        [CTL_KERN, KERN_OSRELEASE] => Some(Value::Str(release())),
        [CTL_KERN, KERN_OSREV] => Some(Value::U32(199_506)),
        [CTL_KERN, KERN_VERSION] => Some(Value::Str(version())),
        [CTL_KERN, KERN_OSVERSION] => Some(Value::Str(build())),
        [CTL_KERN, KERN_HOSTNAME] => Some(Value::Str(hostname())),
        [CTL_KERN, KERN_ARGMAX] => Some(Value::U32(1024 * 1024)),
        [CTL_HW, HW_MODEL] => Some(Value::Str(model())),
        [CTL_HW, HW_NCPU] | [CTL_HW, HW_AVAILCPU] => Some(Value::U32(ncpu())),
        [CTL_HW, HW_BYTEORDER] => Some(Value::U32(1234)),
        [CTL_HW, HW_PAGESIZE] => Some(Value::U32(page_size())),
        [CTL_HW, HW_CACHELINE] => Some(Value::U32(128)),
        [CTL_HW, HW_MEMSIZE] => Some(Value::U64(memsize())),
        // `_PATH_STDPATH`: where a Darwin userland keeps its utilities, which
        // is also where a prefix's base puts them. `whereis` searches it.
        [CTL_USER, USER_CS_PATH] => Some(Value::Str("/usr/bin:/bin:/usr/sbin:/sbin".to_string())),
        _ => None,
    }
}

/// The `kern.*` strings, overridable so a guest can be told it is on a given release.
///
/// The defaults describe macOS 15.0. A program that branches on the OS version needs
/// a *specific* answer far more than it needs a true one, and the environment is where
/// that belongs rather than a rebuild.
#[cfg(not(target_os = "macos"))]
fn release() -> String {
    env_or("ROSETTA3_DARWIN_RELEASE", "24.0.0")
}

/// The macOS version the [`release`] above implies, as `(major, minor, patch)`.
///
/// One derivation rather than a second constant, because a guest that reads
/// `kern.osrelease` and a guest that calls `Gestalt` are asking the same
/// question and must not get two answers. Darwin 20 is macOS 11 and each is one
/// apart after that; before it, every Darwin was some 10.x.
#[cfg(not(target_os = "macos"))]
pub fn macos_version() -> (i32, i32, i32) {
    let release = release();
    let mut parts = release.split('.').map(|p| p.parse::<i32>().unwrap_or(0));
    let darwin = parts.next().unwrap_or(24);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    match darwin >= 20 {
        true => (darwin - 9, minor, patch),
        false => (10, darwin - 4, minor),
    }
}

#[cfg(not(target_os = "macos"))]
fn build() -> String {
    env_or("ROSETTA3_DARWIN_BUILD", "24A335")
}

#[cfg(not(target_os = "macos"))]
fn version() -> String {
    env_or(
        "ROSETTA3_DARWIN_VERSION",
        &format!(
            "Darwin Kernel Version {}: root:xnu-11215.1.10~2/RELEASE_{}",
            release(),
            match guest_arch() {
                GuestArch::X86_64 => "X86_64",
                GuestArch::Arm64 => "ARM64_T8103",
            }
        ),
    )
}

#[cfg(not(target_os = "macos"))]
fn model() -> String {
    env_or("ROSETTA3_DARWIN_MODEL", "Mac15,3")
}

#[cfg(not(target_os = "macos"))]
fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

#[cfg(not(target_os = "macos"))]
fn hostname() -> String {
    // `c_char` rather than `i8`: it is unsigned on aarch64 Linux and signed on x86-64
    // and on macOS, so spelling the sign is a portability bug on one host or another.
    let mut buf = [0 as libc::c_char; 256];
    // SAFETY: `buf` is a live array of exactly the length passed.
    if unsafe { libc::gethostname(buf.as_mut_ptr(), buf.len() - 1) } != 0 {
        return "localhost".to_string();
    }
    // SAFETY: `gethostname` succeeded, so `buf` holds a NUL-terminated name.
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn ncpu() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
}

#[cfg(not(target_os = "macos"))]
fn page_size() -> u32 {
    (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).max(4096) as u32
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn memsize() -> u64 {
    // SAFETY: as above; both parameters are plain counts.
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    if pages <= 0 {
        // Some sandboxes decline to answer. A wrong non-zero number is worse than a
        // conventional one: a guest sizing a cache off zero divides by it.
        return 8 * 1024 * 1024 * 1024;
    }
    pages as u64 * page_size() as u64
}

/// Copy `value` out under the `oldp`/`oldlenp` protocol every `sysctl` key follows.
///
/// A null `oldp` is a size enquiry and succeeds; too small a buffer reports the size
/// it would need and fails with `ENOMEM`, which is what BSD's `sysctl_old_user` does
/// and what a caller sizing a buffer in two passes depends on.
///
/// # Safety
///
/// `oldlenp`, if non-null, is a writable `size_t`, and `oldp`, if non-null, is
/// writable for `*oldlenp` bytes.
unsafe fn copy_out(value: &Value, oldp: *mut libc::c_void, oldlenp: *mut libc::size_t) -> i32 {
    let bytes = value.bytes();
    if oldlenp.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    // SAFETY: the caller guarantees `oldlenp` is a writable `size_t`.
    let available = unsafe { *oldlenp };
    if oldp.is_null() {
        unsafe { *oldlenp = bytes.len() };
        return 0;
    }
    if available < bytes.len() {
        unsafe { *oldlenp = bytes.len() };
        crate::errno::set(libc::ENOMEM);
        return -1;
    }
    // SAFETY: `oldp` is writable for `available` bytes, which is at least `bytes.len()`.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), oldp as *mut u8, bytes.len());
        *oldlenp = bytes.len();
    }
    0
}

/// Report a key nothing here answers, under `ROSETTA3_TRACE_SYSCTL`.
///
/// The list above is narrow on purpose, so the way it grows is by a program naming
/// what it wanted - which it cannot do through an `ENOENT` alone.
fn say_unanswered(what: &std::fmt::Arguments<'_>) {
    if std::env::var_os("ROSETTA3_TRACE_SYSCTL").is_none() {
        return;
    }
    eprintln!("rosetta3: sysctl: no answer for {what}");
}

/// `int sysctl(int *, u_int, void *, size_t *, void *, size_t)`.
///
/// # Safety
///
/// Called only through an import slot this module defines, with the six arguments the
/// declaration gives it.
unsafe extern "C" fn sysctl(
    name: *const libc::c_int,
    namelen: libc::c_uint,
    oldp: *mut libc::c_void,
    oldlenp: *mut libc::size_t,
    newp: *mut libc::c_void,
    newlen: libc::size_t,
) -> i32 {
    if name.is_null() || namelen == 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: the caller states `name` holds `namelen` integers.
    let mib = unsafe { std::slice::from_raw_parts(name, namelen as usize) };

    if let Some(value) = guest_value(mib) {
        // SAFETY: the caller's own `oldp`/`oldlenp`, unmodified.
        return unsafe { copy_out(&value, oldp, oldlenp) };
    }
    // SAFETY: every argument is the caller's, forwarded unchanged.
    unsafe { fallback(mib, oldp, oldlenp, newp, newlen) }
}

/// Everything [`guest_value`] did not claim.
///
/// # Safety
///
/// As [`sysctl`].
#[cfg(target_os = "macos")]
unsafe fn fallback(
    mib: &[libc::c_int],
    oldp: *mut libc::c_void,
    oldlenp: *mut libc::size_t,
    newp: *mut libc::c_void,
    newlen: libc::size_t,
) -> i32 {
    // SAFETY: the host's own `sysctl`, given the caller's arguments unchanged.
    unsafe {
        libc::sysctl(
            mib.as_ptr() as *mut libc::c_int,
            mib.len() as libc::c_uint,
            oldp,
            oldlenp,
            newp,
            newlen,
        )
    }
}

/// # Safety
///
/// As [`sysctl`].
#[cfg(not(target_os = "macos"))]
unsafe fn fallback(
    mib: &[libc::c_int],
    oldp: *mut libc::c_void,
    oldlenp: *mut libc::size_t,
    _newp: *mut libc::c_void,
    _newlen: libc::size_t,
) -> i32 {
    match host_value(mib) {
        // SAFETY: the caller's own `oldp`/`oldlenp`.
        Some(value) => unsafe { copy_out(&value, oldp, oldlenp) },
        None => {
            say_unanswered(&format_args!("mib {mib:?}"));
            crate::errno::set(libc::ENOENT);
            -1
        }
    }
}

/// The `sysctlbyname` names this module knows, and their MIBs.
///
/// Only the keys with a numeric MIB. A name with no MIB - `kern.osproductversion` and
/// the rest of the string-only space - is answered by [`NAMED`].
const BY_NAME: &[(&str, &[i32])] = &[
    ("kern.ostype", &[CTL_KERN, KERN_OSTYPE]),
    ("kern.osrelease", &[CTL_KERN, KERN_OSRELEASE]),
    ("kern.osrevision", &[CTL_KERN, KERN_OSREV]),
    ("kern.version", &[CTL_KERN, KERN_VERSION]),
    ("kern.osversion", &[CTL_KERN, KERN_OSVERSION]),
    ("kern.hostname", &[CTL_KERN, KERN_HOSTNAME]),
    ("kern.argmax", &[CTL_KERN, KERN_ARGMAX]),
    ("hw.machine", &[CTL_HW, HW_MACHINE]),
    ("hw.machinearch", &[CTL_HW, HW_MACHINE_ARCH]),
    ("hw.model", &[CTL_HW, HW_MODEL]),
    ("hw.ncpu", &[CTL_HW, HW_NCPU]),
    ("hw.availcpu", &[CTL_HW, HW_AVAILCPU]),
    ("hw.byteorder", &[CTL_HW, HW_BYTEORDER]),
    ("hw.pagesize", &[CTL_HW, HW_PAGESIZE]),
    ("hw.cachelinesize", &[CTL_HW, HW_CACHELINE]),
    ("hw.memsize", &[CTL_HW, HW_MEMSIZE]),
    ("user.cs_path", &[CTL_USER, USER_CS_PATH]),
];

/// String-only keys, which have no MIB and are reachable by name alone.
#[cfg(not(target_os = "macos"))]
fn named(name: &str) -> Option<Value> {
    match name {
        "kern.osproductversion" => Some(Value::Str(env_or("ROSETTA3_MACOS_VERSION", "15.0"))),
        "kern.osproductversioncompat" => Some(Value::Str(env_or("ROSETTA3_MACOS_VERSION", "15.0"))),
        // Whether *this* process is running under translation. A guest asking is
        // asking about itself, and under this runtime the answer is yes for an Intel
        // image on an arm64 host and no otherwise.
        "sysctl.proc_translated" => Some(Value::U32(u32::from(
            guest_arch() == GuestArch::X86_64 && cfg!(target_arch = "aarch64"),
        ))),
        "hw.optional.arm64" => Some(Value::U32(u32::from(guest_arch() == GuestArch::Arm64))),
        "machdep.cpu.brand_string" => Some(Value::Str(brand_string())),
        "hw.cputype" => Some(Value::U32(guest_arch().cpu_type() as u32)),
        "hw.cpusubtype" => Some(Value::U32(guest_arch().cpu_subtype() as u32)),
        // `CPUFAMILY_UNKNOWN`. The real values are opaque hashes of a specific
        // Apple or Intel part, and a caller that recognises one acts on what it
        // knows about that part - so naming a family this is not is worse than
        // the answer Darwin itself gives for a CPU it does not recognise.
        "hw.cpufamily" | "hw.cpusubfamily" => Some(Value::U32(0)),
        _ => optional_feature(name),
    }
}

/// `hw.optional.*`: which instructions the guest may actually execute.
///
/// The question is not "what can this CPU do". Under emulation the guest's
/// instructions are recompiled, and a guest told the machine has AVX-512 emits
/// AVX-512 that the lifter has to be able to translate - so the honest answer
/// there is the *baseline* of the guest's architecture and nothing more.
///
/// The native backend is the case where the two coincide: the program's
/// instructions are this CPU's, so it may ask this CPU. [`set_runs_host_
/// instructions`] is how that is said, and it is the only thing that turns the
/// wider answers on.
///
/// A name this does not know is left to the caller's `None`, which is
/// `ENOENT` - what a Mac without the feature answers, because Darwin publishes
/// an `hw.optional` key only when the feature is present.
#[cfg(not(target_os = "macos"))]
fn optional_feature(name: &str) -> Option<Value> {
    let x86 = guest_arch() == GuestArch::X86_64;
    let arm = !x86;
    let present = match name {
        // SSE and SSE2 are the x86-64 baseline; no such CPU lacks them.
        "hw.optional.sse" | "hw.optional.sse2" | "hw.optional.floatingpoint" => x86,
        // NEON is the arm64 baseline, likewise.
        "hw.optional.AdvSIMD" | "hw.optional.neon" => arm,
        "hw.optional.sse3" => x86 && host_has("sse3"),
        "hw.optional.supplementalsse3" => x86 && host_has("ssse3"),
        "hw.optional.sse4_1" => x86 && host_has("sse4.1"),
        "hw.optional.sse4_2" => x86 && host_has("sse4.2"),
        "hw.optional.avx1_0" => x86 && host_has("avx"),
        "hw.optional.avx2_0" => x86 && host_has("avx2"),
        "hw.optional.avx512f" => x86 && host_has("avx512f"),
        "hw.optional.f16c" => x86 && host_has("f16c"),
        "hw.optional.fma" => x86 && host_has("fma"),
        // The arm64 feature names, none of which is a baseline. Answered only
        // on a host whose own instructions the guest runs, which for an arm64
        // guest means an arm64 host - and this crate cannot ask an aarch64 CPU
        // for them portably, so they are absent rather than claimed.
        _ if name.starts_with("hw.optional.arm.") => false,
        _ => return None,
    };
    // Absent, not zero: Darwin removes the key rather than answering `0`, and a
    // caller that checks for the key's *existence* - which is the documented way
    // to read these - would read a zero as "present".
    present.then_some(Value::U32(1))
}

/// Whether the guest's instructions are this CPU's own, and so whether
/// `hw.optional` may report what this CPU has. See [`optional_feature`].
#[cfg(not(target_os = "macos"))]
static RUNS_HOST_INSTRUCTIONS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Say that the program executes this CPU's instructions directly, which is what
/// the native backend does and the emulated one does not.
#[cfg(not(target_os = "macos"))]
pub fn set_runs_host_instructions(native: bool) {
    RUNS_HOST_INSTRUCTIONS.store(native, Ordering::Release);
}

/// Whether the program executes this CPU's instructions directly - which is
/// what makes a guest function pointer something host code can call.
#[cfg(not(target_os = "macos"))]
pub fn runs_host_instructions() -> bool {
    RUNS_HOST_INSTRUCTIONS.load(Ordering::Acquire)
}

#[cfg(not(target_os = "macos"))]
fn host_has(feature: &str) -> bool {
    if !RUNS_HOST_INSTRUCTIONS.load(Ordering::Acquire) {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    {
        return match feature {
            "sse3" => std::arch::is_x86_feature_detected!("sse3"),
            "ssse3" => std::arch::is_x86_feature_detected!("ssse3"),
            "sse4.1" => std::arch::is_x86_feature_detected!("sse4.1"),
            "sse4.2" => std::arch::is_x86_feature_detected!("sse4.2"),
            "avx" => std::arch::is_x86_feature_detected!("avx"),
            "avx2" => std::arch::is_x86_feature_detected!("avx2"),
            "avx512f" => std::arch::is_x86_feature_detected!("avx512f"),
            "f16c" => std::arch::is_x86_feature_detected!("f16c"),
            "fma" => std::arch::is_x86_feature_detected!("fma"),
            _ => false,
        };
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = feature;
        false
    }
}

/// `machdep.cpu.brand_string` - the CPU's marketing name.
#[cfg(not(target_os = "macos"))]
fn brand_string() -> String {
    fn from_cpuinfo() -> Option<String> {
        let text = std::fs::read_to_string("/proc/cpuinfo").ok()?;
        for line in text.lines() {
            let (key, value) = line.split_once(':')?;
            if matches!(key.trim(), "model name" | "Model name" | "Processor") {
                return Some(value.trim().to_string());
            }
        }
        None
    }
    env_or_else("ROSETTA3_CPU_BRAND", || {
        from_cpuinfo().unwrap_or_else(|| "Unknown CPU".to_string())
    })
}

#[cfg(not(target_os = "macos"))]
fn env_or_else(key: &str, fallback: impl FnOnce() -> String) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback())
}

/// `int sysctlbyname(const char *, void *, size_t *, void *, size_t)`.
///
/// # Safety
///
/// As [`sysctl`], with `name` a NUL-terminated string.
unsafe extern "C" fn sysctlbyname(
    name: *const libc::c_char,
    oldp: *mut libc::c_void,
    oldlenp: *mut libc::size_t,
    newp: *mut libc::c_void,
    newlen: libc::size_t,
) -> i32 {
    if name.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    // SAFETY: the caller states `name` is NUL-terminated.
    let key = unsafe { std::ffi::CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();

    if let Some((_, mib)) = BY_NAME.iter().find(|(n, _)| *n == key) {
        // SAFETY: `mib` is a static slice of the right length; the rest is the
        // caller's.
        return unsafe {
            sysctl(
                mib.as_ptr(),
                mib.len() as libc::c_uint,
                oldp,
                oldlenp,
                newp,
                newlen,
            )
        };
    }
    // SAFETY: every argument is the caller's, forwarded unchanged.
    unsafe { by_name_fallback(&key, oldp, oldlenp, newp, newlen) }
}

/// # Safety
///
/// As [`sysctlbyname`].
#[cfg(target_os = "macos")]
unsafe fn by_name_fallback(
    key: &str,
    oldp: *mut libc::c_void,
    oldlenp: *mut libc::size_t,
    newp: *mut libc::c_void,
    newlen: libc::size_t,
) -> i32 {
    let Ok(c) = std::ffi::CString::new(key) else {
        crate::errno::set(libc::EINVAL);
        return -1;
    };
    // SAFETY: `c` outlives the call; the rest is the caller's.
    unsafe { libc::sysctlbyname(c.as_ptr(), oldp, oldlenp, newp, newlen) }
}

/// # Safety
///
/// As [`sysctlbyname`].
#[cfg(not(target_os = "macos"))]
unsafe fn by_name_fallback(
    key: &str,
    oldp: *mut libc::c_void,
    oldlenp: *mut libc::size_t,
    _newp: *mut libc::c_void,
    _newlen: libc::size_t,
) -> i32 {
    match named(key) {
        // SAFETY: the caller's own `oldp`/`oldlenp`.
        Some(value) => unsafe { copy_out(&value, oldp, oldlenp) },
        None => {
            say_unanswered(&format_args!("name {key:?}"));
            crate::errno::set(libc::ENOENT);
            -1
        }
    }
}

/// `int sysctlnametomib(const char *, int *, size_t *)`.
///
/// # Safety
///
/// `name` is NUL-terminated, `sizep` a writable `size_t`, and `mibp` writable for
/// `*sizep` integers.
unsafe extern "C" fn sysctlnametomib(
    name: *const libc::c_char,
    mibp: *mut libc::c_int,
    sizep: *mut libc::size_t,
) -> i32 {
    if name.is_null() || mibp.is_null() || sizep.is_null() {
        crate::errno::set(libc::EFAULT);
        return -1;
    }
    // SAFETY: the caller states `name` is NUL-terminated.
    let key = unsafe { std::ffi::CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    let Some((_, mib)) = BY_NAME.iter().find(|(n, _)| *n == key) else {
        say_unanswered(&format_args!("name {key:?} as a mib"));
        crate::errno::set(libc::ENOENT);
        return -1;
    };
    // SAFETY: the caller states `sizep` is a writable `size_t`.
    let available = unsafe { *sizep };
    if available < mib.len() {
        crate::errno::set(libc::ENOMEM);
        return -1;
    }
    // SAFETY: `mibp` is writable for `available` integers, which is at least
    // `mib.len()`.
    unsafe {
        std::ptr::copy_nonoverlapping(mib.as_ptr(), mibp, mib.len());
        *sizep = mib.len();
    }
    0
}

exports::exports! {
    functions {
        "sysctl" => sysctl,
        "sysctlbyname" => sysctlbyname,
        "sysctlnametomib" => sysctlnametomib,
    }
}

/// Serialises the tests that set [`GUEST_ARCH`].
///
/// The architecture is process-global because it is a property of the one program this
/// process is running - which is right for a runtime and wrong for a test harness,
/// where several tests set it at once on different threads. Shared with
/// [`crate::sysinfo`]'s tests, which read the same global through `uname`.
#[cfg(test)]
pub(crate) static ARCH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`ARCH_LOCK`] and set the architecture, for the duration of the caller.
#[cfg(test)]
pub(crate) fn lock_guest_arch(arch: GuestArch) -> std::sync::MutexGuard<'static, ()> {
    let guard = ARCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_guest_arch(arch);
    guard
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ask a key the way a guest does, and get the bytes back.
    fn ask(mib: &[i32]) -> Option<Vec<u8>> {
        let mut len: libc::size_t = 0;
        // SAFETY: a null `oldp` with a writable length is the size enquiry every
        // `sysctl` key answers.
        let rc = unsafe {
            sysctl(
                mib.as_ptr(),
                mib.len() as libc::c_uint,
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        // SAFETY: `buf` is exactly the length the enquiry above reported.
        let rc = unsafe {
            sysctl(
                mib.as_ptr(),
                mib.len() as libc::c_uint,
                buf.as_mut_ptr() as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (rc == 0).then(|| {
            buf.truncate(len);
            buf
        })
    }

    fn ask_str(mib: &[i32]) -> Option<String> {
        let mut bytes = ask(mib)?;
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        String::from_utf8(bytes).ok()
    }

    #[test]
    fn user_cs_path_is_the_darwin_standard_path() {
        assert_eq!(
            ask_str(&[CTL_USER, USER_CS_PATH]).as_deref(),
            Some("/usr/bin:/bin:/usr/sbin:/sbin")
        );
    }

    /// The same answer on both hosts, which is the property that makes a Linux run
    /// comparable to a macOS one.
    #[test]
    fn the_kernel_is_darwin_on_either_host() {
        assert_eq!(ask_str(&[CTL_KERN, KERN_OSTYPE]).as_deref(), Some("Darwin"));
    }

    /// The key this whole module exists for: `hw.machine` is a fact about the guest,
    /// so an Intel guest reads `x86_64` even on an arm64 Mac where the host's own
    /// `sysctl` would say `arm64`.
    #[test]
    fn the_machine_is_the_guest_s_and_not_the_host_s() {
        let _held = lock_guest_arch(GuestArch::X86_64);
        assert_eq!(ask_str(&[CTL_HW, HW_MACHINE]).as_deref(), Some("x86_64"));
        assert_eq!(ask_str(&[CTL_HW, HW_MACHINE_ARCH]).as_deref(), Some("i386"));

        set_guest_arch(GuestArch::Arm64);
        assert_eq!(ask_str(&[CTL_HW, HW_MACHINE]).as_deref(), Some("arm64"));
    }

    /// A caller sizing its buffer in two passes needs the failing first pass to
    /// report the size it would need, and to have written nothing.
    ///
    /// Asked of `hw.machine`, which this module answers on both hosts. A key that is
    /// forwarded to a real Darwin `sysctl` is that implementation's to get right, and
    /// its truncation behaviour is not this module's to assert.
    #[test]
    fn too_small_a_buffer_reports_the_size_it_needs() {
        let _held = lock_guest_arch(GuestArch::X86_64);
        let mib = [CTL_HW, HW_MACHINE];
        let mut buf = [0xAAu8; 2];
        let mut len: libc::size_t = buf.len();
        // SAFETY: `buf` is live for `len` bytes, which is what is passed.
        let rc = unsafe {
            sysctl(
                mib.as_ptr(),
                2,
                buf.as_mut_ptr() as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(rc, -1);
        assert_eq!(crate::errno::get(), libc::ENOMEM);
        assert_eq!(len, "x86_64".len() + 1, "the size it would have needed");
        assert_eq!(buf, [0xAA; 2], "and nothing was written");
        crate::errno::set(0);
    }

    #[test]
    fn a_key_nothing_answers_is_enoent_rather_than_a_guess() {
        let mut len: libc::size_t = 0;
        // SAFETY: a size enquiry against a MIB with no meaning.
        let rc = unsafe {
            sysctl(
                [99, 99].as_ptr(),
                2,
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(rc, -1);
        crate::errno::set(0);
    }

    #[test]
    fn a_name_round_trips_through_its_mib() {
        let name = std::ffi::CString::new("hw.machine").unwrap();
        let mut mib = [0i32; 8];
        let mut len: libc::size_t = mib.len();
        // SAFETY: `mib` is live for `len` integers and `name` outlives the call.
        let rc = unsafe { sysctlnametomib(name.as_ptr(), mib.as_mut_ptr(), &mut len) };
        assert_eq!(rc, 0);
        assert_eq!(&mib[..len], &[CTL_HW, HW_MACHINE]);
    }

    #[test]
    fn by_name_and_by_mib_agree() {
        let _held = lock_guest_arch(GuestArch::X86_64);
        let name = std::ffi::CString::new("hw.machine").unwrap();
        let mut buf = [0u8; 32];
        let mut len: libc::size_t = buf.len();
        // SAFETY: `buf` is live for `len` bytes and `name` outlives the call.
        let rc = unsafe {
            sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(rc, 0);
        assert_eq!(&buf[..len - 1], b"x86_64");
    }

    /// Every name in the table must map to a MIB this module actually answers, or
    /// the two halves have drifted.
    #[test]
    fn every_named_key_has_an_answer() {
        let _held = lock_guest_arch(GuestArch::X86_64);
        for (name, mib) in BY_NAME {
            assert!(ask(mib).is_some(), "{name} has a MIB but no answer");
        }
    }
}
