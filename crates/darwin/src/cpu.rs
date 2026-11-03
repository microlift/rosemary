//! The x86-64 processor the guest sees, described once.
//!
//! `CPUID` is the source of truth: the emulated backend answers the guest's
//! `CPUID` from [`cpuid`], and `sysctl`'s `machdep.cpu.*` and `hw.optional.*`
//! and the comm page's capability bits are all decoded from it, so a guest
//! that asks the same question two ways gets one answer.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the guest's instructions are this CPU's own.
static RUNS_HOST_INSTRUCTIONS: AtomicBool = AtomicBool::new(false);

/// Say that the program executes this CPU's instructions directly, which is what
/// the native backend does and the emulated one does not.
pub fn set_runs_host_instructions(native: bool) {
    RUNS_HOST_INSTRUCTIONS.store(native, Ordering::Release);
}

pub fn runs_host_instructions() -> bool {
    RUNS_HOST_INSTRUCTIONS.load(Ordering::Acquire)
}

/// What `CPUID` returns to an x86-64 guest, as `[eax, ebx, ecx, edx]`.
///
/// A guest running natively executes the instruction itself, so this asks the
/// same CPU; otherwise it is the emulated processor's answer.
pub fn cpuid(leaf: u32, subleaf: u32) -> [u32; 4] {
    #[cfg(target_arch = "x86_64")]
    if runs_host_instructions() {
        let r = std::arch::x86_64::__cpuid_count(leaf, subleaf);
        return [r.eax, r.ebx, r.ecx, r.edx];
    }
    emulated::cpuid(leaf, subleaf)
}

/// A leaf's sub-leaf 0, or zeros past the highest leaf of its range: what lies
/// beyond it is unspecified, and Intel parts repeat their last leaf there.
fn leaf(leaf: u32) -> [u32; 4] {
    let highest = cpuid(leaf & EXTENDED_LEAVES, 0)[EAX];
    match leaf <= highest {
        true => cpuid(leaf, 0),
        false => [0; 4],
    }
}

const EAX: usize = 0;
const EBX: usize = 1;
const ECX: usize = 2;
const EDX: usize = 3;

const VENDOR_LEAF: u32 = 0x0;
const VERSION_LEAF: u32 = 0x1;
const STRUCTURED_FEATURES_LEAF: u32 = 0x7;
const EXTENDED_LEAVES: u32 = 0x8000_0000;
const EXTENDED_FEATURES_LEAF: u32 = 0x8000_0001;
const BRAND_LEAVES: std::ops::RangeInclusive<u32> = 0x8000_0002..=0x8000_0004;

/// The feature flags anything here reads, by where `CPUID` reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    Fpu,
    Mmx,
    Sse,
    Sse2,
    Sse3,
    Ssse3,
    Fma,
    Sse4_1,
    Sse4_2,
    Aes,
    Avx,
    F16c,
    Rdrand,
    Bmi1,
    Hle,
    Avx2,
    Bmi2,
    Erms,
    Rtm,
    Avx512f,
    Avx512dq,
    Rdseed,
    Adx,
    Avx512ifma,
    Avx512cd,
    Avx512bw,
    Avx512vl,
    Avx512vbmi,
    Vaes,
    Vpclmulqdq,
    Avx512vnni,
    Avx512bitalg,
    Avx512vpopcntdq,
    LongMode,
}

impl Feature {
    /// `(leaf, register, bit)`, as the Intel SDM's `CPUID` tables give them.
    const fn location(self) -> (u32, usize, u32) {
        use Feature::*;
        match self {
            Fpu => (VERSION_LEAF, EDX, 0),
            Mmx => (VERSION_LEAF, EDX, 23),
            Sse => (VERSION_LEAF, EDX, 25),
            Sse2 => (VERSION_LEAF, EDX, 26),
            Sse3 => (VERSION_LEAF, ECX, 0),
            Ssse3 => (VERSION_LEAF, ECX, 9),
            Fma => (VERSION_LEAF, ECX, 12),
            Sse4_1 => (VERSION_LEAF, ECX, 19),
            Sse4_2 => (VERSION_LEAF, ECX, 20),
            Aes => (VERSION_LEAF, ECX, 25),
            Avx => (VERSION_LEAF, ECX, 28),
            F16c => (VERSION_LEAF, ECX, 29),
            Rdrand => (VERSION_LEAF, ECX, 30),
            Bmi1 => (STRUCTURED_FEATURES_LEAF, EBX, 3),
            Hle => (STRUCTURED_FEATURES_LEAF, EBX, 4),
            Avx2 => (STRUCTURED_FEATURES_LEAF, EBX, 5),
            Bmi2 => (STRUCTURED_FEATURES_LEAF, EBX, 8),
            Erms => (STRUCTURED_FEATURES_LEAF, EBX, 9),
            Rtm => (STRUCTURED_FEATURES_LEAF, EBX, 11),
            Avx512f => (STRUCTURED_FEATURES_LEAF, EBX, 16),
            Avx512dq => (STRUCTURED_FEATURES_LEAF, EBX, 17),
            Rdseed => (STRUCTURED_FEATURES_LEAF, EBX, 18),
            Adx => (STRUCTURED_FEATURES_LEAF, EBX, 19),
            Avx512ifma => (STRUCTURED_FEATURES_LEAF, EBX, 21),
            Avx512cd => (STRUCTURED_FEATURES_LEAF, EBX, 28),
            Avx512bw => (STRUCTURED_FEATURES_LEAF, EBX, 30),
            Avx512vl => (STRUCTURED_FEATURES_LEAF, EBX, 31),
            Avx512vbmi => (STRUCTURED_FEATURES_LEAF, ECX, 1),
            Vaes => (STRUCTURED_FEATURES_LEAF, ECX, 9),
            Vpclmulqdq => (STRUCTURED_FEATURES_LEAF, ECX, 10),
            Avx512vnni => (STRUCTURED_FEATURES_LEAF, ECX, 11),
            Avx512bitalg => (STRUCTURED_FEATURES_LEAF, ECX, 12),
            Avx512vpopcntdq => (STRUCTURED_FEATURES_LEAF, ECX, 14),
            LongMode => (EXTENDED_FEATURES_LEAF, EDX, 29),
        }
    }
}

pub fn has(feature: Feature) -> bool {
    let (number, register, bit) = feature.location();
    leaf(number)[register] & (1 << bit) != 0
}

/// `machdep.cpu.vendor`: the twelve bytes leaf 0 spells across `ebx`, `edx`, `ecx`.
pub fn vendor() -> String {
    let words = leaf(VENDOR_LEAF);
    let bytes: Vec<u8> = [words[EBX], words[EDX], words[ECX]]
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// `machdep.cpu.brand_string`, without the padding parts put around it.
pub fn brand() -> String {
    let bytes: Vec<u8> = BRAND_LEAVES
        .flat_map(leaf)
        .flat_map(|word| word.to_le_bytes())
        .collect();
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_owned()
}

/// `machdep.cpu.signature`: leaf 1's `eax`, which the fields below are cut from.
pub fn signature() -> u32 {
    leaf(VERSION_LEAF)[EAX]
}

pub fn stepping() -> u32 {
    signature() & 0xf
}

pub fn family() -> u32 {
    (signature() >> 8) & 0xf
}

pub fn extended_family() -> u32 {
    (signature() >> 20) & 0xff
}

pub fn extended_model() -> u32 {
    (signature() >> 16) & 0xf
}

/// `machdep.cpu.model`: the extended model is folded in only for families 6 and
/// 15, which is the rule XNU's `cpuid_set_generic_info` follows.
pub fn model() -> u32 {
    const P6_FAMILY: u32 = 0x6;
    const NETBURST_FAMILY: u32 = 0xf;
    let model = (signature() >> 4) & 0xf;
    match family() {
        P6_FAMILY | NETBURST_FAMILY => model + (extended_model() << 4),
        _ => model,
    }
}

/// XNU's `quad(hi, lo)`, which is how it packs two feature registers.
fn quad(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// `machdep.cpu.feature_bits`.
pub fn feature_bits() -> u64 {
    let words = leaf(VERSION_LEAF);
    quad(words[ECX], words[EDX])
}

/// `machdep.cpu.extfeature_bits`.
pub fn extfeature_bits() -> u64 {
    let words = leaf(EXTENDED_FEATURES_LEAF);
    quad(words[ECX], words[EDX])
}

/// `machdep.cpu.leaf7_feature_bits`.
pub fn leaf7_feature_bits() -> u64 {
    let words = leaf(STRUCTURED_FEATURES_LEAF);
    quad(words[ECX], words[EBX])
}

/// The `CLFLUSH` line size, which leaf 1 counts in eight-byte units.
pub fn cache_line_size() -> u32 {
    ((leaf(VERSION_LEAF)[EBX] >> 8) & 0xff) * 8
}

/// The nominal frequency in hertz, as an Intel brand string states it
/// (`... @ 2.67GHz`), which is the figure an Intel Mac's `hw.cpufrequency` holds.
pub fn nominal_frequency() -> Option<u64> {
    let brand = brand();
    let (_, rated) = brand.rsplit_once('@')?;
    let rated = rated.trim();
    let (number, scale) = [("GHz", 1e9), ("MHz", 1e6)]
        .into_iter()
        .find_map(|(unit, scale)| Some((rated.strip_suffix(unit)?, scale)))?;
    let value: f64 = number.trim().parse().ok()?;
    Some((value * scale).round() as u64)
}

/// The processor the emulated backend presents: the baseline Microlift lifts,
/// and no more.
pub mod emulated {
    /// The highest basic leaf answered. Leaves above it read as zero.
    pub const MAX_BASIC_LEAF: u32 = 1;

    /// The highest extended leaf answered: the last quarter of the brand string.
    pub const MAX_EXTENDED_LEAF: u32 = 0x8000_0004;

    /// The processor brand string, `NUL`-padded to the 48 bytes leaves
    /// `0x80000002..=0x80000004` spell out.
    const BRAND: [u8; 48] = brand();

    const fn brand() -> [u8; 48] {
        let name = b"rosemary (microlift x86-64 on arm64) @ 2.67GHz";
        let mut out = [0u8; 48];
        let mut i = 0;
        while i < name.len() {
            out[i] = name[i];
            i += 1;
        }
        out
    }

    pub fn cpuid(leaf: u32, subleaf: u32) -> [u32; 4] {
        /// A four-byte slice of the brand string, as the little-endian word the
        /// register holds.
        fn brand_word(offset: usize) -> u32 {
            u32::from_le_bytes([
                BRAND[offset],
                BRAND[offset + 1],
                BRAND[offset + 2],
                BRAND[offset + 3],
            ])
        }

        match leaf {
            // Vendor String: "GenuineIntel"
            0x0 => [
                MAX_BASIC_LEAF,
                u32::from_le_bytes(*b"Genu"),
                u32::from_le_bytes(*b"ntel"),
                u32::from_le_bytes(*b"ineI"),
            ],

            // Family / Model / Stepping & Standard Features
            0x1 => {
                // Family 6, Model 44 (0x2C), Stepping 0.
                let version = 0x0002_06C0;

                // Cache line size (64 bytes -> 8 units of 8-bytes) in bits 15:8
                let misc = 8 << 8;

                // FPU | TSC | CX8 | CMOV | SSE | SSE2
                let edx = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 15) | (1 << 25) | (1 << 26);

                // SSE3 (bit 0) | SSSE3 (bit 9) | SSE4.1 (bit 19) | SSE4.2 (bit 20) | AES-NI (bit 25) | Hypervisor (bit 31)
                let ecx = (1 << 0) | (1 << 9) | (1 << 19) | (1 << 20) | (1 << 25) | (1 << 31);

                [version, misc, ecx, edx]
            }

            // Leaf 4: Deterministic Cache Parameters
            0x4 => match subleaf {
                // L1 Data Cache (128 KB, 64-byte line, 8-way)
                0 => {
                    let eax = (1 << 0) | (1 << 5) | (1 << 8); // Type: Data (1), Level: 1, Self Initializing
                    let ebx = (63 << 0) | (0 << 12) | (7 << 22); // Line size (63+1=64), Partitions (0+1=1), Ways (7+1=8)
                    let ecx = 255; // Sets (255+1=256) -> 256 * 64 * 1 * 8 = 128 KB
                    let edx = 0;
                    [eax, ebx, ecx, edx]
                }
                // L1 Instruction Cache (128 KB, 64-byte line, 8-way)
                1 => {
                    let eax = (2 << 0) | (1 << 5) | (1 << 8); // Type: Instruction (2), Level: 1, Self Initializing
                    let ebx = (63 << 0) | (0 << 12) | (7 << 22); // Line size 64, 8-way
                    let ecx = 255; // Sets 256 -> 128 KB
                    let edx = 0;
                    [eax, ebx, ecx, edx]
                }
                // L2 Unified Cache (8192 KB / 8 MB, 64-byte line, 16-way)
                2 => {
                    let eax = (3 << 0) | (2 << 5) | (1 << 8); // Type: Unified (3), Level: 2, Self Initializing
                    let ebx = (63 << 0) | (0 << 12) | (15 << 22); // Line size 64, Ways (15+1=16)
                    let ecx = 8191; // Sets (8191+1=8192) -> 8192 * 64 * 1 * 16 = 8192 KB
                    let edx = 0;
                    [eax, ebx, ecx, edx]
                }
                // No more caches
                _ => [0, 0, 0, 0],
            },

            // Leaf 7: Structured Extended Feature Flags
            0x7 => [0, 0, 0, 0], // Sub-leaf 0: No AVX2, BMI, AVX-512, SHA, etc.

            // Extended Leaves
            0x8000_0000 => [MAX_EXTENDED_LEAF, 0, 0, 0],

            // Extended Processor Info (64-bit Long Mode enabled)
            0x8000_0001 => [0, 0, 0, 1 << 29],

            // Brand String across leaves 0x80000002 to 0x80000004
            0x8000_0002..=0x8000_0004 => {
                let base = (leaf - 0x8000_0002) as usize * 16;
                [
                    brand_word(base),
                    brand_word(base + 4),
                    brand_word(base + 8),
                    brand_word(base + 12),
                ]
            }

            _ => [0, 0, 0, 0],
        }
    }
}

/// Serialises the tests that set [`RUNS_HOST_INSTRUCTIONS`], which every
/// decoder above reads.
#[cfg(test)]
pub(crate) static BACKEND_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`BACKEND_LOCK`] and pick the backend, for the duration of the caller.
#[cfg(test)]
pub(crate) fn lock_backend(native: bool) -> std::sync::MutexGuard<'static, ()> {
    let guard = BACKEND_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_runs_host_instructions(native);
    guard
}

#[cfg(test)]
mod tests {
    use super::emulated::{MAX_BASIC_LEAF, MAX_EXTENDED_LEAF, cpuid};
    use super::*;

    /// A guest reads the vendor before it reads anything else, and it reads it
    /// out of three registers in an order that is easy to transpose.
    #[test]
    fn leaf_zero_spells_the_vendor() {
        let [eax, ebx, ecx, edx] = cpuid(0, 0);
        let mut vendor = Vec::new();
        vendor.extend_from_slice(&ebx.to_le_bytes());
        vendor.extend_from_slice(&edx.to_le_bytes());
        vendor.extend_from_slice(&ecx.to_le_bytes());
        assert_eq!(&vendor, b"GenuineIntel");
        assert_eq!(eax, MAX_BASIC_LEAF);
    }

    /// The whole point of the reply: SSE2 yes, everything microlift cannot lift no.
    #[test]
    fn leaf_one_advertises_the_baseline_and_no_more() {
        let [_, _, ecx, edx] = cpuid(1, 0);
        for (bit, name) in [
            (0, "fpu"),
            (4, "tsc"),
            (8, "cx8"),
            (25, "sse"),
            (26, "sse2"),
        ] {
            assert_ne!(edx & (1 << bit), 0, "{name} should be advertised");
        }
        // MMX and FXSR: baseline on real hardware, unimplemented here.
        for (bit, name) in [(23, "mmx"), (24, "fxsr")] {
            assert_eq!(edx & (1 << bit), 0, "{name} should not be advertised");
        }
        for (bit, name) in [
            (13, "cmpxchg16b"),
            (23, "popcnt"),
            (26, "xsave"),
            (27, "osxsave"),
            (28, "avx"),
        ] {
            assert_eq!(ecx & (1 << bit), 0, "{name} should not be advertised");
        }
        // Emulated, and saying so.
        assert_ne!(ecx & (1 << 31), 0, "hypervisor bit should be set");
    }

    /// The brand string is one 48-byte block split across twelve registers,
    /// and a misordered word would still look like a plausible string.
    #[test]
    fn brand_string_reassembles() {
        let mut bytes = Vec::new();
        for leaf in 0x8000_0002..=0x8000_0004 {
            for word in cpuid(leaf, 0) {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
        }
        assert_eq!(bytes.len(), 48);
        let text = String::from_utf8(bytes).expect("brand is ascii");
        assert!(
            text.starts_with("rosemary (microlift x86-64 on arm64)"),
            "{text}"
        );
        assert!(text.ends_with('\0'), "brand string is NUL-terminated");
        assert_eq!(cpuid(0x8000_0000, 0)[0], MAX_EXTENDED_LEAF);
    }

    /// Long mode, since the code asking is 64-bit and already running.
    #[test]
    fn extended_leaf_one_reports_long_mode() {
        assert_ne!(cpuid(0x8000_0001, 0)[3] & (1 << 29), 0);
    }

    /// Out of range in either space reads as zero rather than as some other
    /// leaf's feature words, and the sub-leaf changes nothing.
    #[test]
    fn unknown_leaves_are_zero() {
        assert_eq!(cpuid(MAX_BASIC_LEAF + 1, 0), [0, 0, 0, 0]);
        assert_eq!(cpuid(7, 0), [0, 0, 0, 0]);
        assert_eq!(cpuid(0x4000_0000, 0), [0, 0, 0, 0]);
        assert_eq!(cpuid(MAX_EXTENDED_LEAF + 1, 0), [0, 0, 0, 0]);
        assert_eq!(cpuid(1, 0), cpuid(1, 7));
    }

    #[test]
    fn decodes_the_emulated_processor() {
        let _held = lock_backend(false);
        assert_eq!(vendor(), "GenuineIntel");
        assert_eq!((family(), model(), stepping()), (6, 0x2c, 0));
        assert_eq!(cache_line_size(), 64);
        assert_eq!(nominal_frequency(), Some(2_670_000_000));
        assert!(has(Feature::Sse4_2) && has(Feature::LongMode));
        assert!(!has(Feature::Mmx) && !has(Feature::Avx2));
        assert_eq!(feature_bits() & (1 << 26), 1 << 26, "SSE2 in the low word");
        assert_eq!(feature_bits() >> 32, u64::from(cpuid(1, 0)[2]));
    }

    #[test]
    fn a_brand_without_a_rating_has_no_nominal_frequency() {
        let _held = lock_backend(true);
        let rated = brand().contains('@');
        assert_eq!(nominal_frequency().is_some(), rated, "{}", brand());
    }
}
