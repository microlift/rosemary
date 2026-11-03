use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::cpu::{self, Feature};

/// Top-level MIB identifiers.
pub const CTL_KERN: i32 = 1;
const CTL_VM: i32 = 2;
pub const CTL_HW: i32 = 6;
pub const CTL_USER: i32 = 8;
const CTL_SYSCTL: i32 = 0;
const CTL_MACHDEP: i32 = 7;

/// `CTL_KERN` second-level identifiers, for the keys answered here.
pub const KERN_OSTYPE: i32 = 1;
pub const KERN_OSRELEASE: i32 = 2;
pub const KERN_OSREV: i32 = 3;
pub const KERN_VERSION: i32 = 4;
pub const KERN_HOSTNAME: i32 = 10;
pub const KERN_ARGMAX: i32 = 8;
const KERN_MAXPROC: i32 = 6;
const KERN_BOOTTIME: i32 = 21;
pub const KERN_OSVERSION: i32 = 65;
const KERN_OSPRODUCTVERSION: i32 = 138;
const KERN_OSPRODUCTVERSIONCOMPAT: i32 = 137;

/// `CTL_VM` second-level identifiers.
const VM_SWAPUSAGE: i32 = 5;

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

const SYSCTL_PROC_TRANSLATED: i32 = 100;

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

static IS_TRANSLATED: AtomicBool = AtomicBool::new(false);

pub fn set_is_translated(translated: bool) {
    IS_TRANSLATED.store(translated, Ordering::SeqCst);
}

pub fn is_translated() -> bool {
    IS_TRANSLATED.load(Ordering::SeqCst)
}

/// `sysctl.proc_translated`, or `None` where macOS has no such key.
///
/// Only Apple silicon has Rosetta, so an Intel Mac answers `ENOENT`; a program
/// reads that as "native x86-64". An arm64 host is Apple silicon, where
/// a native process reads 0. A translated guest reads 1 on every host, because
/// that is what an x86-64 binary sees on the only Macs that translate.
fn proc_translated() -> Option<u32> {
    match is_translated() {
        true => Some(1),
        false if cfg!(target_arch = "aarch64") => Some(0),
        false => None,
    }
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
enum Value {
    Str(String),
    U32(u32),
    U64(u64),
    Bytes(Vec<u8>),
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
            Value::Bytes(bytes) => bytes.clone(),
        }
    }
}

/// Where a key's answer comes from.
#[derive(Clone, Copy)]
enum Source {
    /// A fact about the guest - its architecture, its processor, the OS it
    /// believes it runs - answered here whatever the host and backend.
    Guest(fn() -> Option<Value>),
    /// A fact about the machine, which a Mac's own kernel answers when
    /// [`passes_through`] and this module answers everywhere else.
    Host(fn() -> Option<Value>),
}

/// A key this module knows: its name, its MIB, and where its answer comes from.
///
/// A key whose answer is `None` does not exist here, by name or by MIB: Darwin
/// removes a key rather than answering a placeholder, and a key nobody has
/// needed yet is better as an `ENOENT` naming itself under
/// `ROSEMARY_TRACE_SYSCTL` than as a plausible invention.
struct Key {
    name: &'static str,
    mib: &'static [i32],
    source: Source,
}

impl Key {
    /// Whether this module answers the key, rather than a Mac's kernel.
    fn is_own(&self) -> bool {
        matches!(self.source, Source::Guest(_)) || !passes_through()
    }

    fn value(&self) -> Option<Value> {
        match self.source {
            Source::Guest(answer) | Source::Host(answer) => answer(),
        }
    }
}

/// The key this module answers at `mib`.
fn own_by_mib(mib: &[i32]) -> Option<&'static Key> {
    KEYS.iter().find(|key| key.mib == mib && key.is_own())
}

/// The key this module answers as `name`.
fn own_by_name(name: &str) -> Option<&'static Key> {
    KEYS.iter().find(|key| key.name == name && key.is_own())
}

/// Whether a key this module does not answer goes to the host's own `sysctl`.
///
/// Only a Mac has one to ask, and only the native libc asks it: builtin is
/// meant to behave the same on every host, so it keeps to this module's table.
fn passes_through() -> bool {
    cfg!(target_os = "macos") && !crate::libc_backend::using_builtin()
}

/// `CTL_HW` keys Darwin numbers at boot (`OID_AUTO`); these are this module's
/// numbers for them, from the same range.
const HW_OPTIONAL: i32 = 102;
const HW_CPUTYPE: i32 = 109;
const HW_CPUSUBTYPE: i32 = 110;
const HW_CPU64BIT_CAPABLE: i32 = 111;
const HW_CPUFAMILY: i32 = 112;
const HW_CPUSUBFAMILY: i32 = 113;
const HW_PHYSICALCPU: i32 = 114;
const HW_PHYSICALCPU_MAX: i32 = 115;
const HW_LOGICALCPU: i32 = 116;
const HW_LOGICALCPU_MAX: i32 = 117;
const HW_PACKAGES: i32 = 118;
const HW_CPUFREQUENCY: i32 = 119;
const HW_ACTIVECPU: i32 = 120;

/// `machdep.cpu` and its leaves, likewise numbered here.
const MACHDEP_CPU: i32 = 100;
const MACHDEP_CPU_VENDOR: i32 = 101;
const MACHDEP_CPU_FAMILY: i32 = 102;
const MACHDEP_CPU_MODEL: i32 = 103;
const MACHDEP_CPU_BRAND_STRING: i32 = 104;
const MACHDEP_CPU_STEPPING: i32 = 105;
const MACHDEP_CPU_SIGNATURE: i32 = 106;
const MACHDEP_CPU_FEATURE_BITS: i32 = 107;
const MACHDEP_CPU_EXTFEATURE_BITS: i32 = 108;
const MACHDEP_CPU_LEAF7_FEATURE_BITS: i32 = 109;
const MACHDEP_CPU_EXTMODEL: i32 = 110;
const MACHDEP_CPU_EXTFAMILY: i32 = 111;
const MACHDEP_CPU_CORE_COUNT: i32 = 112;
const MACHDEP_CPU_THREAD_COUNT: i32 = 113;

/// The value of an x86-64 processor's key, which an arm64 guest's Mac does not have.
fn x86(answer: impl FnOnce() -> Value) -> Option<Value> {
    (guest_arch() == GuestArch::X86_64).then(answer)
}

/// An x86 `hw.optional` key: 0 or 1, as XNU's `sysctl_cpu_capability` answers
/// from the comm page's capability bits, which [`crate::cpu`] also decodes.
fn x86_feature(feature: Feature) -> Option<Value> {
    x86(|| Value::U32(u32::from(cpu::has(feature))))
}

/// An arm64 `hw.optional` key that is the architecture's baseline.
fn arm64_baseline() -> Option<Value> {
    (guest_arch() == GuestArch::Arm64).then_some(Value::U32(1))
}

/// Every key this module knows.
const KEYS: &[Key] = &[
    // The OS the guest believes it is on is a fixed identity, not the host's: a
    // program comparing `uname`, `kern.osproductversion` and
    // `_availability_version_check` against one another, or against what it
    // saw on another host, must get the one answer from all of them.
    Key {
        name: "kern.ostype",
        mib: &[CTL_KERN, KERN_OSTYPE],
        source: Source::Guest(|| Some(Value::Str("Darwin".to_string()))),
    },
    Key {
        name: "kern.osrelease",
        mib: &[CTL_KERN, KERN_OSRELEASE],
        source: Source::Guest(|| Some(Value::Str(release()))),
    },
    Key {
        name: "kern.osrevision",
        mib: &[CTL_KERN, KERN_OSREV],
        source: Source::Guest(|| Some(Value::U32(KERN_OSREV_VALUE))),
    },
    Key {
        name: "kern.version",
        mib: &[CTL_KERN, KERN_VERSION],
        source: Source::Guest(|| Some(Value::Str(version()))),
    },
    Key {
        name: "kern.osversion",
        mib: &[CTL_KERN, KERN_OSVERSION],
        source: Source::Guest(|| Some(Value::Str(prefix::release::build().to_string()))),
    },
    Key {
        name: "kern.osproductversion",
        mib: &[CTL_KERN, KERN_OSPRODUCTVERSION],
        source: Source::Guest(|| Some(Value::Str(prefix::release::product_version()))),
    },
    Key {
        name: "kern.osproductversioncompat",
        mib: &[CTL_KERN, KERN_OSPRODUCTVERSIONCOMPAT],
        source: Source::Guest(|| Some(Value::Str(prefix::release::product_version()))),
    },
    Key {
        name: "kern.hostname",
        mib: &[CTL_KERN, KERN_HOSTNAME],
        source: Source::Host(|| Some(Value::Str(hostname()))),
    },
    Key {
        name: "kern.argmax",
        mib: &[CTL_KERN, KERN_ARGMAX],
        source: Source::Host(|| Some(Value::U32(ARG_MAX))),
    },
    Key {
        name: "kern.maxproc",
        mib: &[CTL_KERN, KERN_MAXPROC],
        source: Source::Host(|| starnix::system::max_processes().map(Value::U32)),
    },
    Key {
        name: "kern.boottime",
        mib: &[CTL_KERN, KERN_BOOTTIME],
        source: Source::Host(boot_time),
    },
    Key {
        name: "vm.swapusage",
        mib: &[CTL_VM, VM_SWAPUSAGE],
        source: Source::Host(swap_usage),
    },
    // `_PATH_STDPATH`: where a Darwin userland keeps its utilities, which is
    // also where a prefix's base puts them. `whereis` searches it.
    Key {
        name: "user.cs_path",
        mib: &[CTL_USER, USER_CS_PATH],
        source: Source::Host(|| Some(Value::Str("/usr/bin:/bin:/usr/sbin:/sbin".to_string()))),
    },
    Key {
        name: "sysctl.proc_translated",
        mib: &[CTL_SYSCTL, SYSCTL_PROC_TRANSLATED],
        source: Source::Guest(|| proc_translated().map(Value::U32)),
    },
    // Without a pid, the asking process's own. A Mac numbers this key at boot,
    // so its kernel is asked by name.
    Key {
        name: "sysctl.proc_cputype",
        mib: &[CTL_SYSCTL, SYSCTL_PROC_CPUTYPE],
        source: Source::Host(|| Some(Value::U32(guest_arch().cpu_type() as u32))),
    },
    Key {
        name: "hw.machine",
        mib: &[CTL_HW, HW_MACHINE],
        source: Source::Guest(|| Some(Value::Str(guest_arch().machine().to_string()))),
    },
    Key {
        name: "hw.machinearch",
        mib: &[CTL_HW, HW_MACHINE_ARCH],
        source: Source::Guest(|| Some(Value::Str(guest_arch().machine_arch().to_string()))),
    },
    Key {
        name: "hw.model",
        mib: &[CTL_HW, HW_MODEL],
        source: Source::Host(|| Some(Value::Str(model()))),
    },
    Key {
        name: "hw.ncpu",
        mib: &[CTL_HW, HW_NCPU],
        source: Source::Host(|| Some(Value::U32(ncpu()))),
    },
    Key {
        name: "hw.availcpu",
        mib: &[CTL_HW, HW_AVAILCPU],
        source: Source::Host(|| Some(Value::U32(ncpu()))),
    },
    Key {
        name: "hw.activecpu",
        mib: &[CTL_HW, HW_ACTIVECPU],
        source: Source::Host(|| Some(Value::U32(ncpu()))),
    },
    Key {
        name: "hw.logicalcpu",
        mib: &[CTL_HW, HW_LOGICALCPU],
        source: Source::Host(|| Some(Value::U32(ncpu()))),
    },
    Key {
        name: "hw.logicalcpu_max",
        mib: &[CTL_HW, HW_LOGICALCPU_MAX],
        source: Source::Host(|| Some(Value::U32(configured_cpus()))),
    },
    Key {
        name: "hw.physicalcpu",
        mib: &[CTL_HW, HW_PHYSICALCPU],
        source: Source::Host(|| Some(Value::U32(physical_cpus()))),
    },
    Key {
        name: "hw.physicalcpu_max",
        mib: &[CTL_HW, HW_PHYSICALCPU_MAX],
        source: Source::Host(|| Some(Value::U32(physical_cpus()))),
    },
    Key {
        name: "hw.packages",
        mib: &[CTL_HW, HW_PACKAGES],
        source: Source::Host(|| starnix::system::packages().map(Value::U32)),
    },
    Key {
        name: "machdep.cpu.core_count",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_CORE_COUNT],
        source: Source::Host(|| Some(Value::U32(physical_cpus()))),
    },
    Key {
        name: "machdep.cpu.thread_count",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_THREAD_COUNT],
        source: Source::Host(|| Some(Value::U32(ncpu()))),
    },
    Key {
        name: "hw.byteorder",
        mib: &[CTL_HW, HW_BYTEORDER],
        source: Source::Host(|| Some(Value::U32(LITTLE_ENDIAN))),
    },
    Key {
        name: "hw.pagesize",
        mib: &[CTL_HW, HW_PAGESIZE],
        source: Source::Host(|| Some(Value::U32(page_size()))),
    },
    Key {
        name: "hw.memsize",
        mib: &[CTL_HW, HW_MEMSIZE],
        source: Source::Host(|| Some(Value::U64(memsize()))),
    },
    Key {
        name: "hw.cachelinesize",
        mib: &[CTL_HW, HW_CACHELINE],
        source: Source::Guest(|| Some(Value::U32(cache_line_size()))),
    },
    Key {
        name: "hw.cputype",
        mib: &[CTL_HW, HW_CPUTYPE],
        source: Source::Guest(|| Some(Value::U32(guest_arch().cpu_type() as u32))),
    },
    Key {
        name: "hw.cpusubtype",
        mib: &[CTL_HW, HW_CPUSUBTYPE],
        source: Source::Guest(|| Some(Value::U32(guest_arch().cpu_subtype() as u32))),
    },
    // Both guest architectures are 64-bit. A caller that reads a missing key
    // as 0 decides it is on 32-bit i386.
    Key {
        name: "hw.cpu64bit_capable",
        mib: &[CTL_HW, HW_CPU64BIT_CAPABLE],
        source: Source::Guest(|| Some(Value::U32(1))),
    },
    // `CPUFAMILY_UNKNOWN`, which XNU itself answers for a part it does not list.
    Key {
        name: "hw.cpufamily",
        mib: &[CTL_HW, HW_CPUFAMILY],
        source: Source::Host(|| Some(Value::U32(0))),
    },
    Key {
        name: "hw.cpusubfamily",
        mib: &[CTL_HW, HW_CPUSUBFAMILY],
        source: Source::Host(|| Some(Value::U32(0))),
    },
    // Apple silicon publishes no clock, so only an x86-64 guest has one.
    Key {
        name: "hw.cpufrequency",
        mib: &[CTL_HW, HW_CPUFREQUENCY],
        source: Source::Guest(|| {
            let hz = cpu::nominal_frequency().or_else(starnix::system::cpu_frequency);
            hz.and_then(|hz| x86(|| Value::U64(hz)))
        }),
    },
    Key {
        name: "machdep.cpu.brand_string",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_BRAND_STRING],
        source: Source::Guest(|| brand_string().map(Value::Str)),
    },
    Key {
        name: "machdep.cpu.vendor",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_VENDOR],
        source: Source::Guest(|| x86(|| Value::Str(cpu::vendor()))),
    },
    Key {
        name: "machdep.cpu.family",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_FAMILY],
        source: Source::Guest(|| x86(|| Value::U32(cpu::family()))),
    },
    Key {
        name: "machdep.cpu.model",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_MODEL],
        source: Source::Guest(|| x86(|| Value::U32(cpu::model()))),
    },
    Key {
        name: "machdep.cpu.extmodel",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_EXTMODEL],
        source: Source::Guest(|| x86(|| Value::U32(cpu::extended_model()))),
    },
    Key {
        name: "machdep.cpu.extfamily",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_EXTFAMILY],
        source: Source::Guest(|| x86(|| Value::U32(cpu::extended_family()))),
    },
    Key {
        name: "machdep.cpu.stepping",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_STEPPING],
        source: Source::Guest(|| x86(|| Value::U32(cpu::stepping()))),
    },
    Key {
        name: "machdep.cpu.signature",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_SIGNATURE],
        source: Source::Guest(|| x86(|| Value::U32(cpu::signature()))),
    },
    Key {
        name: "machdep.cpu.feature_bits",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_FEATURE_BITS],
        source: Source::Guest(|| x86(|| Value::U64(cpu::feature_bits()))),
    },
    Key {
        name: "machdep.cpu.extfeature_bits",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_EXTFEATURE_BITS],
        source: Source::Guest(|| x86(|| Value::U64(cpu::extfeature_bits()))),
    },
    Key {
        name: "machdep.cpu.leaf7_feature_bits",
        mib: &[CTL_MACHDEP, MACHDEP_CPU, MACHDEP_CPU_LEAF7_FEATURE_BITS],
        source: Source::Guest(|| x86(|| Value::U64(cpu::leaf7_feature_bits()))),
    },
    // Floating point is in both architectures' baseline.
    Key {
        name: "hw.optional.floatingpoint",
        mib: &[CTL_HW, HW_OPTIONAL, 101],
        source: Source::Guest(|| Some(Value::U32(1))),
    },
    Key {
        name: "hw.optional.arm64",
        mib: &[CTL_HW, HW_OPTIONAL, 144],
        source: Source::Guest(|| Some(Value::U32(u32::from(guest_arch() == GuestArch::Arm64)))),
    },
    Key {
        name: "hw.optional.neon",
        mib: &[CTL_HW, HW_OPTIONAL, 131],
        source: Source::Guest(arm64_baseline),
    },
    Key {
        name: "hw.optional.AdvSIMD",
        mib: &[CTL_HW, HW_OPTIONAL, 132],
        source: Source::Guest(arm64_baseline),
    },
    Key {
        name: "hw.optional.x86_64",
        mib: &[CTL_HW, HW_OPTIONAL, 100],
        source: Source::Guest(|| x86_feature(Feature::LongMode)),
    },
    Key {
        name: "hw.optional.mmx",
        mib: &[CTL_HW, HW_OPTIONAL, 102],
        source: Source::Guest(|| x86_feature(Feature::Mmx)),
    },
    Key {
        name: "hw.optional.sse",
        mib: &[CTL_HW, HW_OPTIONAL, 103],
        source: Source::Guest(|| x86_feature(Feature::Sse)),
    },
    Key {
        name: "hw.optional.sse2",
        mib: &[CTL_HW, HW_OPTIONAL, 104],
        source: Source::Guest(|| x86_feature(Feature::Sse2)),
    },
    Key {
        name: "hw.optional.sse3",
        mib: &[CTL_HW, HW_OPTIONAL, 105],
        source: Source::Guest(|| x86_feature(Feature::Sse3)),
    },
    Key {
        name: "hw.optional.supplementalsse3",
        mib: &[CTL_HW, HW_OPTIONAL, 106],
        source: Source::Guest(|| x86_feature(Feature::Ssse3)),
    },
    Key {
        name: "hw.optional.sse4_1",
        mib: &[CTL_HW, HW_OPTIONAL, 107],
        source: Source::Guest(|| x86_feature(Feature::Sse4_1)),
    },
    Key {
        name: "hw.optional.sse4_2",
        mib: &[CTL_HW, HW_OPTIONAL, 108],
        source: Source::Guest(|| x86_feature(Feature::Sse4_2)),
    },
    Key {
        name: "hw.optional.aes",
        mib: &[CTL_HW, HW_OPTIONAL, 109],
        source: Source::Guest(|| x86_feature(Feature::Aes)),
    },
    Key {
        name: "hw.optional.avx1_0",
        mib: &[CTL_HW, HW_OPTIONAL, 111],
        source: Source::Guest(|| x86_feature(Feature::Avx)),
    },
    Key {
        name: "hw.optional.rdrand",
        mib: &[CTL_HW, HW_OPTIONAL, 112],
        source: Source::Guest(|| x86_feature(Feature::Rdrand)),
    },
    Key {
        name: "hw.optional.f16c",
        mib: &[CTL_HW, HW_OPTIONAL, 113],
        source: Source::Guest(|| x86_feature(Feature::F16c)),
    },
    Key {
        name: "hw.optional.enfstrg",
        mib: &[CTL_HW, HW_OPTIONAL, 114],
        source: Source::Guest(|| x86_feature(Feature::Erms)),
    },
    Key {
        name: "hw.optional.fma",
        mib: &[CTL_HW, HW_OPTIONAL, 115],
        source: Source::Guest(|| x86_feature(Feature::Fma)),
    },
    Key {
        name: "hw.optional.avx2_0",
        mib: &[CTL_HW, HW_OPTIONAL, 116],
        source: Source::Guest(|| x86_feature(Feature::Avx2)),
    },
    Key {
        name: "hw.optional.bmi1",
        mib: &[CTL_HW, HW_OPTIONAL, 117],
        source: Source::Guest(|| x86_feature(Feature::Bmi1)),
    },
    Key {
        name: "hw.optional.bmi2",
        mib: &[CTL_HW, HW_OPTIONAL, 118],
        source: Source::Guest(|| x86_feature(Feature::Bmi2)),
    },
    Key {
        name: "hw.optional.rtm",
        mib: &[CTL_HW, HW_OPTIONAL, 119],
        source: Source::Guest(|| x86_feature(Feature::Rtm)),
    },
    Key {
        name: "hw.optional.hle",
        mib: &[CTL_HW, HW_OPTIONAL, 120],
        source: Source::Guest(|| x86_feature(Feature::Hle)),
    },
    Key {
        name: "hw.optional.adx",
        mib: &[CTL_HW, HW_OPTIONAL, 121],
        source: Source::Guest(|| x86_feature(Feature::Adx)),
    },
    Key {
        name: "hw.optional.avx512f",
        mib: &[CTL_HW, HW_OPTIONAL, 124],
        source: Source::Guest(|| x86_feature(Feature::Avx512f)),
    },
    Key {
        name: "hw.optional.avx512cd",
        mib: &[CTL_HW, HW_OPTIONAL, 125],
        source: Source::Guest(|| x86_feature(Feature::Avx512cd)),
    },
    Key {
        name: "hw.optional.avx512dq",
        mib: &[CTL_HW, HW_OPTIONAL, 126],
        source: Source::Guest(|| x86_feature(Feature::Avx512dq)),
    },
    Key {
        name: "hw.optional.avx512bw",
        mib: &[CTL_HW, HW_OPTIONAL, 127],
        source: Source::Guest(|| x86_feature(Feature::Avx512bw)),
    },
    Key {
        name: "hw.optional.avx512vl",
        mib: &[CTL_HW, HW_OPTIONAL, 128],
        source: Source::Guest(|| x86_feature(Feature::Avx512vl)),
    },
    Key {
        name: "hw.optional.avx512ifma",
        mib: &[CTL_HW, HW_OPTIONAL, 129],
        source: Source::Guest(|| x86_feature(Feature::Avx512ifma)),
    },
    Key {
        name: "hw.optional.avx512vbmi",
        mib: &[CTL_HW, HW_OPTIONAL, 130],
        source: Source::Guest(|| x86_feature(Feature::Avx512vbmi)),
    },
];

/// `kern.osrevision`: `BSD`'s `199506`, which XNU still reports.
const KERN_OSREV_VALUE: u32 = 199_506;

/// `ARG_MAX` in `<sys/syslimits.h>`.
const ARG_MAX: u32 = 1024 * 1024;

/// `hw.byteorder`'s spelling of little-endian, as `<machine/endian.h>` has it.
const LITTLE_ENDIAN: u32 = 1234;

/// `machdep.cpu.brand_string`: the x86-64 guest's from `CPUID`, an arm64 one's
/// from the host, which is the only processor it can name.
fn brand_string() -> Option<String> {
    match guest_arch() {
        GuestArch::X86_64 => Some(cpu::brand()),
        GuestArch::Arm64 => starnix::system::cpu_brand(),
    }
}

/// `hw.cachelinesize`: what `CPUID` says for an x86-64 guest, and Apple
/// silicon's 128 bytes for an arm64 one.
fn cache_line_size() -> u32 {
    const APPLE_SILICON_LINE_SIZE: u32 = 128;
    match guest_arch() {
        GuestArch::X86_64 => cpu::cache_line_size(),
        GuestArch::Arm64 => APPLE_SILICON_LINE_SIZE,
    }
}

/// `kern.boottime`: a `struct timeval`, padded to its 16 bytes.
fn boot_time() -> Option<Value> {
    let since = starnix::system::boot_time()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let mut bytes = (since.as_secs() as i64).to_ne_bytes().to_vec();
    bytes.extend_from_slice(&(since.subsec_micros() as i32).to_ne_bytes());
    bytes.resize(size_of::<libc::timeval>().max(16), 0);
    Some(Value::Bytes(bytes))
}

/// `vm.swapusage`: a `struct xsw_usage`.
fn swap_usage() -> Option<Value> {
    let swap = starnix::system::swap()?;
    let mut bytes = Vec::with_capacity(32);
    for field in [swap.total, swap.free, swap.total - swap.free] {
        bytes.extend_from_slice(&field.to_ne_bytes());
    }
    bytes.extend_from_slice(&page_size().to_ne_bytes());
    // `xsu_encrypted`, which this layer has no way to know of the host's swap.
    bytes.extend_from_slice(&0u32.to_ne_bytes());
    Some(Value::Bytes(bytes))
}

/// The `kern.*` release strings, the same on every host; see [`prefix::release`].
fn release() -> String {
    prefix::release::darwin().to_string()
}

/// The macOS version the guest is told it runs, as `(major, minor, patch)`.
pub fn macos_version() -> (i32, i32, i32) {
    prefix::release::macos_version()
}

fn version() -> String {
    env_or(
        "ROSEMARY_DARWIN_VERSION",
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

fn model() -> String {
    env_or("ROSEMARY_DARWIN_MODEL", "Mac15,3")
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

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

pub(crate) fn ncpu() -> u32 {
    std::thread::available_parallelism().map_or(1, |n| n.get() as u32)
}

/// Every processor the machine has, online or not.
pub(crate) fn configured_cpus() -> u32 {
    // SAFETY: `sysconf` reads a count and touches no memory of ours.
    let configured = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) };
    u32::try_from(configured).unwrap_or(0).max(ncpu())
}

/// Cores, as opposed to SMT threads. A host that cannot tell the two apart
/// answers what a machine without SMT would.
pub(crate) fn physical_cpus() -> u32 {
    let logical = ncpu();
    starnix::system::physical_cores().map_or(logical, |cores| cores.min(logical))
}

pub(crate) fn page_size() -> u32 {
    (unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).max(4096) as u32
}

#[cfg(target_os = "macos")]
pub(crate) fn memsize() -> u64 {
    let mut size = 0u64;
    let mut len = size_of::<u64>();
    unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&raw mut size).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    size
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

/// Report a key nothing here answers, under `ROSEMARY_TRACE_SYSCTL`.
///
/// The list above is narrow on purpose, so the way it grows is by a program naming
/// what it wanted - which it cannot do through an `ENOENT` alone.
fn say_unanswered(what: &std::fmt::Arguments<'_>) {
    if std::env::var_os("ROSEMARY_TRACE_SYSCTL").is_none() {
        return;
    }
    eprintln!("rosemary: sysctl: no answer for {what}");
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

    // SAFETY: every argument is the caller's, forwarded unchanged.
    if let Some(rc) = unsafe { describe_tree(mib, oldp, oldlenp, newp, newlen) } {
        return rc;
    }
    if let Some(key) = own_by_mib(mib) {
        // SAFETY: the caller's own `oldp`/`oldlenp`.
        return unsafe { answer(key.value(), oldp, oldlenp) };
    }
    if !passes_through()
        && let Some(answer) = process_value(mib)
    {
        return match answer {
            // SAFETY: the caller's own `oldp`/`oldlenp`.
            Ok(bytes) => unsafe { copy_out(&Value::Bytes(bytes), oldp, oldlenp) },
            Err(error) => {
                crate::errno::set(error);
                -1
            }
        };
    }
    #[cfg(target_os = "macos")]
    if passes_through() {
        // SAFETY: the host's own `sysctl`, given the caller's arguments unchanged.
        return unsafe {
            libc::sysctl(
                mib.as_ptr() as *mut libc::c_int,
                mib.len() as libc::c_uint,
                oldp,
                oldlenp,
                newp,
                newlen,
            )
        };
    }
    say_unanswered(&format_args!("mib {mib:?}"));
    crate::errno::set(libc::ENOENT);
    -1
}

/// Copy out a key's value, or fail with `ENOENT` for a key that does not exist
/// here.
///
/// # Safety
///
/// As [`copy_out`].
unsafe fn answer(value: Option<Value>, oldp: *mut libc::c_void, oldlenp: *mut libc::size_t) -> i32 {
    match value {
        // SAFETY: the caller's own `oldp`/`oldlenp`.
        Some(value) => unsafe { copy_out(&value, oldp, oldlenp) },
        None => {
            crate::errno::set(libc::ENOENT);
            -1
        }
    }
}

/// `CTL_SYSCTL` nodes that describe the tree itself. The `sysctl` command
/// resolves every name through `NAME2OID` and asks `OIDFMT` how to print it,
/// so without these it calls every key unknown.
const SYSCTL_NAME: i32 = 1;
const SYSCTL_NAME2OID: i32 = 3;
const SYSCTL_OIDFMT: i32 = 4;

/// `CTLTYPE_*` and `CTLFLAG_RD`, as `<sys/sysctl.h>` numbers them.
const CTLTYPE_INT: u32 = 2;
const CTLTYPE_STRING: u32 = 3;
const CTLTYPE_QUAD: u32 = 4;
const CTLTYPE_OPAQUE: u32 = 5;
const CTLFLAG_RD: u32 = 0x8000_0000;

/// The tree-introspection nodes, for the keys this module answers.
///
/// `None` for anything else, which a Mac's kernel then answers about its own
/// keys when [`passes_through`]. A key with no value is absent from these
/// nodes too, so the `sysctl` command reports it as an unknown OID rather
/// than printing a value.
///
/// # Safety
///
/// As [`sysctl`].
unsafe fn describe_tree(
    mib: &[i32],
    oldp: *mut libc::c_void,
    oldlenp: *mut libc::size_t,
    newp: *mut libc::c_void,
    newlen: libc::size_t,
) -> Option<i32> {
    let answer = match *mib {
        [CTL_SYSCTL, SYSCTL_NAME2OID] => {
            if newp.is_null() {
                return None;
            }
            // SAFETY: the caller states `newp` is readable for `newlen` bytes.
            let bytes = unsafe { std::slice::from_raw_parts(newp as *const u8, newlen) };
            // The command passes `strlen`, but a terminator is harmless.
            let name = bytes.split(|&b| b == 0).next().unwrap_or_default();
            let key = own_by_name(std::str::from_utf8(name).ok()?)?;
            key.value()
                .map(|_| Value::Bytes(key.mib.iter().flat_map(|n| n.to_ne_bytes()).collect()))
        }
        [CTL_SYSCTL, SYSCTL_NAME, ref oid @ ..] => {
            let key = own_by_mib(oid)?;
            key.value().map(|_| Value::Str(key.name.to_string()))
        }
        [CTL_SYSCTL, SYSCTL_OIDFMT, ref oid @ ..] => own_by_mib(oid)?.value().map(|value| {
            let (kind, format) = match value {
                Value::Str(_) => (CTLTYPE_STRING, "A"),
                Value::U32(_) => (CTLTYPE_INT, "I"),
                Value::U64(_) => (CTLTYPE_QUAD, "Q"),
                Value::Bytes(_) => (CTLTYPE_OPAQUE, ""),
            };
            let mut bytes = (kind | CTLFLAG_RD).to_ne_bytes().to_vec();
            bytes.extend_from_slice(format.as_bytes());
            bytes.push(0);
            Value::Bytes(bytes)
        }),
        _ => return None,
    };
    // SAFETY: the caller's own `oldp`/`oldlenp`.
    Some(unsafe { self::answer(answer, oldp, oldlenp) })
}

/// `CTL_KERN` keys that describe processes.
const KERN_PROC: i32 = 14;
const KERN_PROCARGS: i32 = 38;
const KERN_PROCARGS2: i32 = 49;

/// `sysctl.proc_cputype`, a dynamically numbered key on a Mac; this is the
/// number this module gives it. A pid follows it in the MIB.
const SYSCTL_PROC_CPUTYPE: i32 = 101;

/// Keys answered from the process table: another process's record, its
/// arguments, its CPU type.
///
/// `None` for a key this does not own; `Some(Err(errno))` for one it owns and
/// refuses - a pid that is not a visible guest is `EINVAL` for the argument
/// keys, as on a Mac, and an empty list for `KERN_PROC`.
fn process_value(mib: &[i32]) -> Option<Result<Vec<u8>, i32>> {
    match *mib {
        [CTL_KERN, KERN_PROC, selector] => {
            Some(crate::process::select(selector, None).ok_or(libc::EINVAL))
        }
        [CTL_KERN, KERN_PROC, selector, argument] => {
            Some(crate::process::select(selector, Some(argument)).ok_or(libc::EINVAL))
        }
        [CTL_KERN, KERN_PROCARGS2, pid] => Some(
            crate::process::describe(pid)
                .map(|process| crate::process::procargs2(&process))
                .ok_or(libc::EINVAL),
        ),
        // The older layout is the newer one without the leading `argc`.
        [CTL_KERN, KERN_PROCARGS, pid] => Some(
            crate::process::describe(pid)
                .map(|process| crate::process::procargs2(&process)[4..].to_vec())
                .ok_or(libc::EINVAL),
        ),
        [CTL_SYSCTL, SYSCTL_PROC_CPUTYPE, pid] => Some(
            crate::process::describe(pid)
                .map(|process| process.cputype.to_ne_bytes().to_vec())
                .ok_or(libc::ESRCH),
        ),
        _ => None,
    }
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
    let key = unsafe { std::ffi::CStr::from_ptr(name) };
    let spelled = key.to_string_lossy();
    if let Some(key) = own_by_name(&spelled) {
        // SAFETY: the caller's own `oldp`/`oldlenp`.
        return unsafe { answer(key.value(), oldp, oldlenp) };
    }
    #[cfg(target_os = "macos")]
    if passes_through() {
        // SAFETY: the caller's arguments, unchanged.
        return unsafe { libc::sysctlbyname(key.as_ptr(), oldp, oldlenp, newp, newlen) };
    }
    let _ = (newp, newlen);
    say_unanswered(&format_args!("name {spelled:?}"));
    crate::errno::set(libc::ENOENT);
    -1
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
    let spelled = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy();
    let Some(key) = own_by_name(&spelled) else {
        #[cfg(target_os = "macos")]
        if passes_through() {
            // SAFETY: the caller's arguments, unchanged.
            return unsafe { libc::sysctlnametomib(name, mibp, sizep) };
        }
        say_unanswered(&format_args!("name {spelled:?} as a mib"));
        crate::errno::set(libc::ENOENT);
        return -1;
    };
    if key.value().is_none() {
        crate::errno::set(libc::ENOENT);
        return -1;
    }
    // SAFETY: the caller states `sizep` is a writable `size_t`.
    let available = unsafe { *sizep };
    if available < key.mib.len() {
        crate::errno::set(libc::ENOMEM);
        return -1;
    }
    // SAFETY: `mibp` is writable for `available` integers, which is at least
    // `key.mib.len()`.
    unsafe {
        std::ptr::copy_nonoverlapping(key.mib.as_ptr(), mibp, key.mib.len());
        *sizep = key.mib.len();
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

    fn ask_str_name(name: &str) -> Option<String> {
        let mut bytes = ask_name(name)?;
        while bytes.last() == Some(&0) {
            bytes.pop();
        }
        String::from_utf8(bytes).ok()
    }

    fn ask_name(name: &str) -> Option<Vec<u8>> {
        let name = std::ffi::CString::new(name).unwrap();
        let mut len: libc::size_t = 0;
        let rc = unsafe {
            sysctlbyname(
                name.as_ptr(),
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
        let rc = unsafe {
            sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
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
    fn proc_translated_agrees_by_name_and_resolved_mib() {
        let _held = ARCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_is_translated(true);
        let name = std::ffi::CString::new("sysctl.proc_translated").unwrap();
        let mut mib = [0i32; 24];
        let mut len = mib.len();
        assert_eq!(
            unsafe { sysctlnametomib(name.as_ptr(), mib.as_mut_ptr(), &mut len) },
            0
        );
        assert_eq!(&mib[..len], &[CTL_SYSCTL, SYSCTL_PROC_TRANSLATED]);

        let one = Some(1u32.to_ne_bytes().to_vec());
        assert_eq!(ask_name("sysctl.proc_translated"), one);
        assert_eq!(ask(&mib[..len]), one);
        set_is_translated(false);
    }

    #[test]
    fn a_native_process_is_untranslated_only_where_rosetta_exists() {
        let _held = ARCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_is_translated(false);
        let answer = ask_name("sysctl.proc_translated");
        let by_mib = ask(&[CTL_SYSCTL, SYSCTL_PROC_TRANSLATED]);
        let name = std::ffi::CString::new("sysctl.proc_translated").unwrap();
        let mut mib = [0i32; 4];
        let mut len = mib.len();
        let resolved = unsafe { sysctlnametomib(name.as_ptr(), mib.as_mut_ptr(), &mut len) };
        match cfg!(target_arch = "aarch64") {
            true => {
                assert_eq!(answer, Some(0u32.to_ne_bytes().to_vec()));
                assert_eq!(by_mib, answer);
                assert_eq!(resolved, 0);
            }
            // An Intel Mac has no such key.
            false => {
                assert_eq!(answer, None);
                assert_eq!(by_mib, None);
                assert_eq!(resolved, -1);
            }
        }
        crate::errno::set(0);
    }

    /// Resolve a name the way the `sysctl` command does, through `NAME2OID`,
    /// and read back its name and format.
    fn ask_tree(name: &str) -> Option<(Vec<i32>, String, u32, String)> {
        let mut oid = [0i32; 24];
        let mut len = size_of_val(&oid);
        let rc = unsafe {
            sysctl(
                [CTL_SYSCTL, SYSCTL_NAME2OID].as_ptr(),
                2,
                oid.as_mut_ptr().cast(),
                &mut len,
                name.as_ptr() as *mut libc::c_void,
                name.len(),
            )
        };
        if rc != 0 {
            return None;
        }
        let oid = oid[..len / size_of::<i32>()].to_vec();
        let query = |node: i32| {
            let mut mib = vec![CTL_SYSCTL, node];
            mib.extend_from_slice(&oid);
            ask(&mib).unwrap()
        };
        let mut named = query(SYSCTL_NAME);
        named.pop();
        let format = query(SYSCTL_OIDFMT);
        let kind = u32::from_ne_bytes(format[..4].try_into().unwrap());
        let spelled = String::from_utf8(format[4..format.len() - 1].to_vec()).unwrap();
        Some((oid, String::from_utf8(named).unwrap(), kind, spelled))
    }

    #[test]
    fn the_sysctl_command_can_resolve_and_format_a_key() {
        let _held = lock_guest_arch(GuestArch::X86_64);
        set_is_translated(true);
        assert_eq!(
            ask_tree("sysctl.proc_translated"),
            Some((
                vec![CTL_SYSCTL, SYSCTL_PROC_TRANSLATED],
                "sysctl.proc_translated".to_string(),
                CTLTYPE_INT | CTLFLAG_RD,
                "I".to_string(),
            ))
        );
        let (_, _, kind, format) = ask_tree("hw.machine").unwrap();
        assert_eq!((kind, format.as_str()), (CTLTYPE_STRING | CTLFLAG_RD, "A"));

        set_is_translated(false);
        let native = ask_tree("sysctl.proc_translated");
        assert_eq!(native.is_some(), cfg!(target_arch = "aarch64"));
        crate::errno::set(0);
    }

    #[test]
    fn guest_cpu_properties_agree_by_name_and_mib() {
        let _held = lock_guest_arch(GuestArch::X86_64);
        for name in ["hw.cputype", "hw.cpusubtype", "hw.optional.arm64"] {
            let mib = KEYS.iter().find(|key| key.name == name).unwrap().mib;
            assert_eq!(ask_name(name), ask(mib), "{name}");
        }
    }

    #[test]
    fn reports_a_64_bit_capable_cpu() {
        assert_eq!(
            ask_name("hw.cpu64bit_capable"),
            Some(1u32.to_ne_bytes().to_vec())
        );
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

    /// A name and its MIB must report the same value or the same absence.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn every_named_key_agrees_with_its_mib() {
        let _held = lock_guest_arch(GuestArch::X86_64);
        for key in KEYS {
            assert_eq!(ask_name(key.name), ask(key.mib), "{}", key.name);
        }
    }

    #[test]
    fn names_and_mibs_are_each_unique() {
        for (i, key) in KEYS.iter().enumerate() {
            for other in &KEYS[i + 1..] {
                assert_ne!(key.name, other.name);
                assert_ne!(key.mib, other.mib, "{} and {}", key.name, other.name);
            }
        }
    }

    /// The emulated processor, read through `sysctl`, is the one its `CPUID`
    /// describes.
    #[test]
    fn the_emulated_processor_is_described_by_its_cpuid() {
        let _held = lock_guest_arch(GuestArch::X86_64);
        let _backend = crate::cpu::lock_backend(false);
        assert_eq!(
            ask_str_name("machdep.cpu.vendor").as_deref(),
            Some("GenuineIntel")
        );
        assert_eq!(
            ask_name("machdep.cpu.signature"),
            Some(crate::cpu::emulated::cpuid(1, 0)[0].to_ne_bytes().to_vec())
        );
        let one = Some(1u32.to_ne_bytes().to_vec());
        let zero = Some(0u32.to_ne_bytes().to_vec());
        assert_eq!(ask_name("hw.optional.sse4_2"), one);
        assert_eq!(
            ask_name("hw.optional.avx2_0"),
            zero,
            "present, and 0, as on an Intel Mac"
        );
        assert_eq!(ask_name("hw.optional.neon"), None);
        assert_eq!(
            ask_name("hw.cpufrequency"),
            Some(2_670_000_000u64.to_ne_bytes().to_vec())
        );
        assert_eq!(
            ask_name("hw.cachelinesize"),
            Some(64u32.to_ne_bytes().to_vec())
        );
    }

    #[test]
    fn an_arm64_guest_has_no_x86_processor_keys() {
        let _held = lock_guest_arch(GuestArch::Arm64);
        assert_eq!(ask_name("machdep.cpu.vendor"), None);
        assert_eq!(ask_name("hw.optional.sse2"), None);
        assert_eq!(ask_name("hw.cpufrequency"), None);
        assert_eq!(
            ask_name("hw.optional.neon"),
            Some(1u32.to_ne_bytes().to_vec())
        );
        set_guest_arch(GuestArch::X86_64);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_topology_counts_agree_with_one_another() {
        let count = |name| u32::from_ne_bytes(ask_name(name).unwrap().try_into().unwrap());
        assert!(count("hw.physicalcpu") >= 1);
        assert!(count("hw.physicalcpu") <= count("hw.logicalcpu"));
        assert!(count("hw.logicalcpu") <= count("hw.logicalcpu_max"));
        assert_eq!(count("hw.logicalcpu"), count("hw.ncpu"));
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn boot_time_is_a_darwin_timeval() {
        let Some(bytes) = ask_name("kern.boottime") else {
            return;
        };
        assert_eq!(bytes.len(), 16);
        let seconds = i64::from_ne_bytes(bytes[..8].try_into().unwrap());
        assert!(seconds > 0);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn swap_usage_is_an_xsw_usage() {
        let Some(bytes) = ask_name("vm.swapusage") else {
            return;
        };
        assert_eq!(bytes.len(), 32);
        let word = |i: usize| u64::from_ne_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap());
        assert_eq!(word(0), word(1) + word(2), "total = available + used");
    }
}
