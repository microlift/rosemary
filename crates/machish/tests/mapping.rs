#![cfg(all(feature = "std", feature = "arch-x86_64"))]

mod common;

use machish::image::{MachObject, map_segments, protect_segments};
use machish::memory::{AddressSpace, BufferSpace, MmapSpace, Placement, ProtectionPolicy};
use machish::spec::MachSpec;
use machish::spec::arch::X86_64;
use machish::spec::constants::MH_MAGIC_64;

const LS: &str = "/bin/ls";

#[test]
fn maps_a_real_executable_into_a_buffer() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();

    // A buffer has no existing mappings, so the preferred base is always
    // honoured - which makes it the right place to assert on exact addresses.
    let mut mem = BufferSpace::new(0);
    let mapped = map_segments(&obj, &mut mem).unwrap();

    let preferred = obj.preferred_base().unwrap();
    assert_eq!(mapped.base, preferred);
    assert_eq!(
        mapped.slide, 0,
        "no slide when the preferred base is available"
    );
    assert_eq!(
        mapped.base, 0x1_0000_0000,
        "x86-64 executables load just past __PAGEZERO"
    );

    // The mapped image begins with the Mach-O header, because __TEXT covers it.
    let mut magic = [0u8; 4];
    mem.read(mapped.base, &mut magic).unwrap();
    assert_eq!(u32::from_le_bytes(magic), MH_MAGIC_64);

    // __PAGEZERO was not mapped: the reservation is a few tens of KiB, not 4 GiB.
    let pagezero = obj
        .segments
        .iter()
        .find(|s| s.name == "__PAGEZERO")
        .unwrap();
    assert_eq!(pagezero.vmsize, 0x1_0000_0000);
    assert!(
        (mapped.span as u64) < 0x100_0000,
        "span {:#x} suggests __PAGEZERO was included",
        mapped.span
    );

    // Every mappable segment's file bytes really landed at its virtual address.
    for seg in obj.mappable_segments() {
        let Some(range) = seg.file_range() else {
            continue;
        };
        let expect = &bytes[..];
        let thin = machish::fat::select(expect, &X86_64).unwrap();
        let src = &thin[range.clone()];

        let mut got = vec![0u8; src.len().min(64)];
        mem.read(seg.vmaddr + mapped.slide, &mut got).unwrap();
        assert_eq!(
            &got[..],
            &src[..got.len()],
            "segment {} content mismatch",
            seg.name
        );
    }

    assert!(mapped.entry.is_some());
    let entry = mapped.entry.unwrap();
    assert!(
        entry >= mapped.base && entry < mapped.base + mapped.span as u64,
        "entry {entry:#x} outside the mapped image"
    );
}

#[test]
fn entry_point_resolves_through_the_slide() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();

    // Map somewhere other than the preferred base and confirm the entry point
    // moves with it. LC_MAIN's entryoff is relative to the image, so a slid
    // image's entry must slide too.
    let mut a = BufferSpace::new(0);
    let unslid = map_segments(&obj, &mut a).unwrap();

    let preferred = obj.preferred_base().unwrap();
    let mut b = BufferSpace::new(0);
    let span = unslid.span;
    let shifted = preferred + 0x1000_0000;
    b.reserve(span, 0x1000, Placement::Preferred(shifted))
        .unwrap();

    assert_eq!(unslid.slide, 0);
    let delta = shifted - preferred;
    assert_eq!(
        unslid.entry.unwrap() + delta,
        machish::image::EntryPoint::address(&obj.entry().unwrap(), shifted, delta),
        "entry must track the slide"
    );
}

#[test]
fn maps_into_host_memory_without_execute_permission() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();

    let mut mem = MmapSpace::new();
    let mapped = map_segments(&obj, &mut mem).unwrap();

    // The host may or may not have 0x1_0000_0000 free; either outcome is fine,
    // but the slide must be consistent with wherever it landed.
    assert_eq!(
        mapped.base.wrapping_sub(obj.preferred_base().unwrap()),
        mapped.slide
    );

    let mut magic = [0u8; 4];
    mem.read(mapped.base, &mut magic).unwrap();
    assert_eq!(u32::from_le_bytes(magic), MH_MAGIC_64);

    // Protection must succeed and must never request PROT_EXEC for a guest
    // image: the arm64 host cannot execute x86-64 under any protection, so the
    // bit would be meaningless. This is the §4.4 invariant.
    let conflicts = protect_segments(&obj, &mut mem, mapped.slide, ProtectionPolicy::Coalesce)
        .expect("coalesced protection must succeed");

    assert!(
        !X86_64.natively_executable(),
        "an x86-64 guest is never host-executable"
    );

    // On a 16 KiB host with a 4 KiB image, __TEXT and __DATA share a page, so
    // at least one conflict is expected and must be reported rather than hidden.
    if mem.protect_granularity() > obj.spec().page_size() {
        assert!(
            !conflicts.is_empty(),
            "expected a reported page conflict at {} KiB granularity",
            mem.protect_granularity() / 1024
        );
        for c in &conflicts {
            assert!(c.segments.len() > 1);
            assert!(!c.applied.exec, "a guest page must never be executable");
        }
    }
}

#[test]
fn protection_policy_none_is_a_no_op() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();

    let mut mem = MmapSpace::new();
    let mapped = map_segments(&obj, &mut mem).unwrap();
    let conflicts = protect_segments(&obj, &mut mem, mapped.slide, ProtectionPolicy::None).unwrap();
    assert!(conflicts.is_empty());

    // Still writable, which is the point of the None policy for a guest whose
    // memory permissions the recompiler enforces (or does not) itself.
    mem.write(mapped.base, &[0u8; 4]).unwrap();
}

#[test]
fn strict_policy_refuses_a_misaligned_image() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();

    let mut mem = MmapSpace::new();
    let mapped = map_segments(&obj, &mut mem).unwrap();

    // A 4 KiB-laid-out image cannot satisfy 16 KiB enforcement. Strict exists
    // to say so out loud rather than silently mis-protect.
    let result = protect_segments(&obj, &mut mem, mapped.slide, ProtectionPolicy::Strict);
    if mem.protect_granularity() > obj.spec().page_size() {
        assert!(
            result.is_err(),
            "strict must reject a sub-granularity layout"
        );
    }
}

#[test]
fn locally_built_binary_maps_and_protects() {
    let Some(bin) = common::build_x86_64("m12_map", "int main(void){return 0;}", &[]) else {
        skip!("cannot build x86_64 binaries on this host")
    };
    let obj = MachObject::parse_any(&bin, X86_64).unwrap();

    let mut mem = BufferSpace::new(0);
    let mapped = map_segments(&obj, &mut mem).unwrap();
    assert_eq!(mapped.slide, 0);
    protect_segments(&obj, &mut mem, mapped.slide, ProtectionPolicy::Coalesce).unwrap();

    let mut magic = [0u8; 4];
    mem.read(mapped.base, &mut magic).unwrap();
    assert_eq!(u32::from_le_bytes(magic), MH_MAGIC_64);
}

/// Laid out as Wine's loader is: the header at `0x2_0000_0000`, with zero-fill
/// segments far below and far above it.
const SPLIT_SOURCE: &str = r#"
__asm__(".zerofill LOW,LOW,_low,0x1000\n.zerofill FAR,FAR,_far,0x10000");
int main(void) { return 0; }
"#;

fn split_executable() -> Option<Vec<u8>> {
    common::build_x86_64(
        "split_segments",
        SPLIT_SOURCE,
        &[
            "-Wl,-pagezero_size,0x1000",
            "-Wl,-image_base,0x200000000",
            "-Wl,-segaddr,LOW,0x10000",
            "-Wl,-segaddr,FAR,0x700000000000",
            "-Wl,-no_pie",
        ],
    )
}

#[test]
fn preferred_base_is_the_header_not_the_lowest_segment() {
    let Some(bytes) = split_executable() else {
        skip!("no x86_64 toolchain")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();
    assert_eq!(obj.preferred_base().unwrap(), 0x2_0000_0000);
}

#[test]
fn maps_segments_far_apart_without_reserving_the_gaps() {
    let Some(bytes) = split_executable() else {
        skip!("no x86_64 toolchain")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();
    let mut mem = BufferSpace::new(0);
    let mapped = map_segments(&obj, &mut mem).unwrap();

    assert_eq!((mapped.base, mapped.slide), (0x2_0000_0000, 0));
    assert!(
        (mapped.span as u64) < 0x100_0000,
        "span {:#x} covers the gaps between segments",
        mapped.span
    );
    let mut magic = [0u8; 4];
    mem.read(mapped.base, &mut magic).unwrap();
    assert_eq!(u32::from_le_bytes(magic), MH_MAGIC_64);
    for addr in [0x1_0000, 0x7000_0000_0000] {
        let mut zero = [0xffu8; 8];
        mem.read(addr, &mut zero).unwrap();
        assert_eq!(zero, [0; 8], "zero-fill segment at {addr:#x}");
    }
    assert!(mem.read(0x1_0000_0000, &mut [0u8; 1]).is_err());
}
