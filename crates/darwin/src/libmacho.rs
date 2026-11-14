//! `<mach-o/swap.h>`'s fat-header swappers from libmacho.
//!
//! libmacho swaps every field unconditionally; the target byte order only
//! documents which way the caller is going.

/// `struct fat_header`, big-endian on disk.
#[repr(C)]
pub struct FatHeader {
    pub magic: u32,
    pub nfat_arch: u32,
}

/// `struct fat_arch`, big-endian on disk.
#[repr(C)]
pub struct FatArch {
    pub cputype: i32,
    pub cpusubtype: i32,
    pub offset: u32,
    pub size: u32,
    pub align: u32,
}

/// `void swap_fat_header(struct fat_header *, enum NXByteOrder)`
///
/// # Safety
///
/// `header` is a writable `struct fat_header`.
pub unsafe extern "C" fn swap_fat_header(header: *mut FatHeader, _target_byte_order: u32) {
    // SAFETY: the caller's header.
    let header = unsafe { &mut *header };
    header.magic = header.magic.swap_bytes();
    header.nfat_arch = header.nfat_arch.swap_bytes();
}

/// `void swap_fat_arch(struct fat_arch *, uint32_t nfat_arch, enum NXByteOrder)`
///
/// # Safety
///
/// `archs` is `nfat_arch` writable `struct fat_arch`es.
pub unsafe extern "C" fn swap_fat_arch(
    archs: *mut FatArch,
    nfat_arch: u32,
    _target_byte_order: u32,
) {
    if nfat_arch == 0 {
        return;
    }
    // SAFETY: the caller's array of `nfat_arch` entries.
    let archs = unsafe { std::slice::from_raw_parts_mut(archs, nfat_arch as usize) };
    for arch in archs {
        arch.cputype = arch.cputype.swap_bytes();
        arch.cpusubtype = arch.cpusubtype.swap_bytes();
        arch.offset = arch.offset.swap_bytes();
        arch.size = arch.size.swap_bytes();
        arch.align = arch.align.swap_bytes();
    }
}

exports::exports! {
    functions {
        "swap_fat_header" => swap_fat_header,
        "swap_fat_arch" => swap_fat_arch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FAT_MAGIC` and `FAT_CIGAM`: the header as read on a little-endian host.
    const FAT_MAGIC: u32 = 0xcafe_babe;
    const FAT_CIGAM: u32 = 0xbeba_feca;
    /// `NX_LittleEndian`.
    const NX_LITTLE_ENDIAN: u32 = 1;

    #[test]
    fn swap_fat_header_reverses_both_fields() {
        let mut header = FatHeader {
            magic: FAT_CIGAM,
            nfat_arch: 2u32.to_be(),
        };
        unsafe { swap_fat_header(&mut header, NX_LITTLE_ENDIAN) };
        assert_eq!(header.magic, FAT_MAGIC);
        assert_eq!(header.nfat_arch, 2);
    }

    #[test]
    fn swap_fat_arch_reverses_every_entry() {
        let entry = |cputype: i32| FatArch {
            cputype: cputype.to_be(),
            cpusubtype: 3i32.to_be(),
            offset: 0x4000u32.to_be(),
            size: 0x1234u32.to_be(),
            align: 14u32.to_be(),
        };
        let mut archs = [entry(0x0100_0007), entry(0x0100_000c)];
        unsafe { swap_fat_arch(archs.as_mut_ptr(), 2, NX_LITTLE_ENDIAN) };
        assert_eq!(
            (archs[0].cputype, archs[1].cputype),
            (0x0100_0007, 0x0100_000c)
        );
        for arch in &archs {
            assert_eq!(
                (arch.cpusubtype, arch.offset, arch.size, arch.align),
                (3, 0x4000, 0x1234, 14)
            );
        }
    }
}
