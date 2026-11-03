//! The x86-64 comm page: the read-only page the Darwin kernel maps at a fixed
//! address in every process, which programs read directly rather than through a
//! call. Qt, for one, reads `_COMM_PAGE_CPU_CAPABILITIES64` while QtCore's
//! initializers run, so a missing page faults before `main`.
//!
//! FIXME (ACCURACY): this is a stub in the manner of Darling's `mldr`. It is filled
//! once at startup and never updated: the nanotime/gettimeofday data are zero (a
//! reader must take the syscall path), memory pressure stays 0, the active CPU
//! count is a snapshot, and the comm page text (preemption-free zone,
//! `_COMM_PAGE_RET`, ...) is not mapped.

use crate::cpu::{Feature, has};
use crate::sysctl::{GuestArch, guest_arch, is_translated};

/// `_COMM_PAGE64_BASE_ADDRESS`.
pub const COMM_PAGE64_BASE_ADDRESS: u64 = 0x0000_7fff_ffe0_0000;
/// `_COMM_PAGE64_AREA_LENGTH`.
const COMM_PAGE64_AREA_LENGTH: usize = 4096;

const COMM_PAGE_SIGNATURE: usize = 0x000;
const COMM_PAGE_CPU_CAPABILITIES64: usize = 0x010;
const COMM_PAGE_VERSION: usize = 0x01e;
const COMM_PAGE_CPU_CAPABILITIES: usize = 0x020;
const COMM_PAGE_NCPUS: usize = 0x022;
const COMM_PAGE_CACHE_LINESIZE: usize = 0x026;
const COMM_PAGE_ACTIVE_CPUS: usize = 0x034;
const COMM_PAGE_PHYSICAL_CPUS: usize = 0x035;
const COMM_PAGE_LOGICAL_CPUS: usize = 0x036;
const COMM_PAGE_MEMORY_SIZE: usize = 0x038;
const COMM_PAGE_KERNEL_PAGE_SHIFT: usize = 0x04d;
const COMM_PAGE_USER_PAGE_SHIFT_64: usize = 0x04e;

const COMM_PAGE_SIGNATURE64: &[u8] = b"commpage 64-bit\0";
const COMM_PAGE_THIS_VERSION: u16 = 14;

// `<i386/cpu_capabilities.h>`.
const K_HAS_MMX: u64 = 0x0000_0001;
const K_HAS_SSE: u64 = 0x0000_0002;
const K_HAS_SSE2: u64 = 0x0000_0004;
const K_HAS_SSE3: u64 = 0x0000_0008;
const K_CACHE64: u64 = 0x0000_0020;
const K_HAS_SUPPLEMENTAL_SSE3: u64 = 0x0000_0100;
const K_64_BIT: u64 = 0x0000_0200;
const K_HAS_SSE4_1: u64 = 0x0000_0400;
const K_HAS_SSE4_2: u64 = 0x0000_0800;
const K_HAS_AES: u64 = 0x0000_1000;
const K_UP: u64 = 0x0000_8000;
const K_NUM_CPUS: u64 = 0x00ff_0000;
const K_NUM_CPUS_SHIFT: u32 = 16;
const K_HAS_AVX1_0: u64 = 0x0100_0000;
const K_HAS_RDRAND: u64 = 0x0200_0000;
const K_HAS_F16C: u64 = 0x0400_0000;
const K_HAS_ENFSTRG: u64 = 0x0800_0000;
const K_HAS_FMA: u64 = 0x1000_0000;
const K_HAS_AVX2_0: u64 = 0x2000_0000;
const K_HAS_BMI1: u64 = 0x4000_0000;
const K_HAS_BMI2: u64 = 0x8000_0000;
const K_HAS_RTM: u64 = 0x0000_0001_0000_0000;
const K_HAS_ADX: u64 = 0x0000_0004_0000_0000;
const K_HAS_RDSEED: u64 = 0x0000_0008_0000_0000;
const K_HAS_AVX512F: u64 = 0x0000_0040_0000_0000;
const K_HAS_AVX512CD: u64 = 0x0000_0080_0000_0000;
const K_HAS_AVX512DQ: u64 = 0x0000_0100_0000_0000;
const K_HAS_AVX512BW: u64 = 0x0000_0200_0000_0000;
const K_HAS_AVX512IFMA: u64 = 0x0000_0400_0000_0000;
const K_HAS_AVX512VBMI: u64 = 0x0000_0800_0000_0000;
const K_HAS_AVX512VL: u64 = 0x0000_1000_0000_0000;
const K_HAS_VAES: u64 = 0x0000_2000_0000_0000;
const K_HAS_VPCLMULQDQ: u64 = 0x0000_4000_0000_0000;
const K_HAS_AVX512VNNI: u64 = 0x0000_8000_0000_0000;
const K_HAS_AVX512BITALG: u64 = 0x0001_0000_0000_0000;
const K_HAS_AVX512VPOPCNTDQ: u64 = 0x0002_0000_0000_0000;
const K_IS_TRANSLATED: u64 = 0x4000_0000_0000_0000;

/// Map and fill the comm page for an x86-64 guest. Does nothing for an arm64
/// guest, whose comm page lives elsewhere and is not provided.
pub fn install() -> std::io::Result<()> {
    if guest_arch() != GuestArch::X86_64 {
        return Ok(());
    }
    let wanted = COMM_PAGE64_BASE_ADDRESS as usize as *mut libc::c_void;
    // A hint rather than a fixed mapping, so that a host which already has
    // something there keeps it and this reports the clash instead.
    // SAFETY: an anonymous private mapping that replaces nothing.
    let page = unsafe {
        libc::mmap(
            wanted,
            COMM_PAGE64_AREA_LENGTH,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if page == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    if page != wanted {
        // SAFETY: `page` is the mapping just made, and nothing refers to it.
        unsafe { libc::munmap(page, COMM_PAGE64_AREA_LENGTH) };
        return Err(std::io::Error::from(std::io::ErrorKind::AddrInUse));
    }
    // SAFETY: `page` is a fresh, writable, zeroed mapping of exactly this length.
    let bytes =
        unsafe { std::slice::from_raw_parts_mut(page.cast::<u8>(), COMM_PAGE64_AREA_LENGTH) };
    fill(bytes);
    // SAFETY: as above; the kernel's comm page is read-only to the process.
    if unsafe { libc::mprotect(page, COMM_PAGE64_AREA_LENGTH, libc::PROT_READ) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn fill(page: &mut [u8]) {
    let mut put = |offset: usize, value: &[u8]| {
        page[offset..offset + value.len()].copy_from_slice(value);
    };
    let ncpus = crate::sysctl::ncpu().min(u8::MAX as u32) as u8;
    let capabilities = cpu_capabilities(ncpus);
    let page_shift = crate::sysctl::page_size().trailing_zeros() as u8;

    put(COMM_PAGE_SIGNATURE, COMM_PAGE_SIGNATURE64);
    put(COMM_PAGE_CPU_CAPABILITIES64, &capabilities.to_le_bytes());
    put(COMM_PAGE_VERSION, &COMM_PAGE_THIS_VERSION.to_le_bytes());
    put(
        COMM_PAGE_CPU_CAPABILITIES,
        &(capabilities as u32).to_le_bytes(),
    );
    // Overlaps `kNumCPUs` in the legacy word above, as on Darwin.
    put(COMM_PAGE_NCPUS, &[ncpus]);
    put(
        COMM_PAGE_CACHE_LINESIZE,
        &(crate::cpu::cache_line_size() as u16).to_le_bytes(),
    );
    put(COMM_PAGE_ACTIVE_CPUS, &[ncpus]);
    put(
        COMM_PAGE_PHYSICAL_CPUS,
        &[crate::sysctl::physical_cpus().min(u8::MAX as u32) as u8],
    );
    put(COMM_PAGE_LOGICAL_CPUS, &[ncpus]);
    put(
        COMM_PAGE_MEMORY_SIZE,
        &crate::sysctl::memsize().to_le_bytes(),
    );
    put(COMM_PAGE_KERNEL_PAGE_SHIFT, &[page_shift]);
    put(COMM_PAGE_USER_PAGE_SHIFT_64, &[page_shift]);
}

/// `_cpu_capabilities`, decoded from the same `CPUID` the guest reads.
fn cpu_capabilities(ncpus: u8) -> u64 {
    let mut capabilities = K_64_BIT | K_CACHE64;
    capabilities |= (u64::from(ncpus) << K_NUM_CPUS_SHIFT) & K_NUM_CPUS;
    if ncpus == 1 {
        capabilities |= K_UP;
    }
    if is_translated() {
        capabilities |= K_IS_TRANSLATED;
    }
    FEATURE_CAPABILITIES
        .iter()
        .filter(|&&(feature, _)| has(feature))
        .fold(capabilities, |capabilities, &(_, bit)| capabilities | bit)
}

/// The capability bits XNU's `commpage_init_cpu_capabilities` sets from `CPUID`.
const FEATURE_CAPABILITIES: &[(Feature, u64)] = &[
    (Feature::Mmx, K_HAS_MMX),
    (Feature::Sse, K_HAS_SSE),
    (Feature::Sse2, K_HAS_SSE2),
    (Feature::Sse3, K_HAS_SSE3),
    (Feature::Ssse3, K_HAS_SUPPLEMENTAL_SSE3),
    (Feature::Sse4_1, K_HAS_SSE4_1),
    (Feature::Sse4_2, K_HAS_SSE4_2),
    (Feature::Aes, K_HAS_AES),
    (Feature::Avx, K_HAS_AVX1_0),
    (Feature::Rdrand, K_HAS_RDRAND),
    (Feature::F16c, K_HAS_F16C),
    (Feature::Erms, K_HAS_ENFSTRG),
    (Feature::Fma, K_HAS_FMA),
    (Feature::Avx2, K_HAS_AVX2_0),
    (Feature::Bmi1, K_HAS_BMI1),
    (Feature::Bmi2, K_HAS_BMI2),
    (Feature::Rtm, K_HAS_RTM),
    (Feature::Adx, K_HAS_ADX),
    (Feature::Rdseed, K_HAS_RDSEED),
    (Feature::Avx512f, K_HAS_AVX512F),
    (Feature::Avx512cd, K_HAS_AVX512CD),
    (Feature::Avx512dq, K_HAS_AVX512DQ),
    (Feature::Avx512bw, K_HAS_AVX512BW),
    (Feature::Avx512ifma, K_HAS_AVX512IFMA),
    (Feature::Avx512vbmi, K_HAS_AVX512VBMI),
    (Feature::Avx512vl, K_HAS_AVX512VL),
    (Feature::Vaes, K_HAS_VAES),
    (Feature::Vpclmulqdq, K_HAS_VPCLMULQDQ),
    (Feature::Avx512vnni, K_HAS_AVX512VNNI),
    (Feature::Avx512bitalg, K_HAS_AVX512BITALG),
    (Feature::Avx512vpopcntdq, K_HAS_AVX512VPOPCNTDQ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_and_versions_the_page() {
        let mut page = vec![0u8; COMM_PAGE64_AREA_LENGTH];
        fill(&mut page);
        assert_eq!(&page[..COMM_PAGE_SIGNATURE64.len()], COMM_PAGE_SIGNATURE64);
        let version = u16::from_le_bytes([page[COMM_PAGE_VERSION], page[COMM_PAGE_VERSION + 1]]);
        assert_eq!(version, COMM_PAGE_THIS_VERSION);
    }

    #[test]
    fn legacy_cpu_count_matches_ncpus_byte() {
        let mut page = vec![0u8; COMM_PAGE64_AREA_LENGTH];
        fill(&mut page);
        let legacy = u32::from_le_bytes(
            page[COMM_PAGE_CPU_CAPABILITIES..COMM_PAGE_CPU_CAPABILITIES + 4]
                .try_into()
                .unwrap(),
        );
        let wide = u64::from_le_bytes(
            page[COMM_PAGE_CPU_CAPABILITIES64..COMM_PAGE_CPU_CAPABILITIES64 + 8]
                .try_into()
                .unwrap(),
        );
        assert_eq!(legacy as u64, wide & u64::from(u32::MAX));
        assert_eq!(
            ((legacy as u64 & K_NUM_CPUS) >> K_NUM_CPUS_SHIFT) as u8,
            page[COMM_PAGE_NCPUS]
        );
        assert!(wide & K_HAS_SSE2 != 0);
    }
}
