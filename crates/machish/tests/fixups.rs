#![cfg(all(feature = "std", feature = "arch-x86_64"))]

mod common;

use std::collections::BTreeMap;

use machish::fixups::{BindKind, Binder, Fixup, LibraryRef, StubMap, decode_image};
use machish::image::{MachObject, map_segments};
use machish::memory::{AddressSpace, BufferSpace};
use machish::report::BindingRecord;
use machish::resolve::{Resolved, SymbolProvider, SyntheticDylib};
use machish::spec::arch::X86_64;

const LS: &str = "/bin/ls";
const ECHO: &str = "/bin/echo";

/// Source used for the cross-encoding comparison. Deliberately touches several
/// libSystem functions and a couple of globals, so both a bind stream and a
/// rebase stream are non-empty.
const AGREE_SRC: &str = r#"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>

static const char *messages[] = { "alpha", "beta", "gamma" };
int counter = 0;

int main(int argc, char **argv) {
    (void)argv;
    for (int i = 0; i < 3; i++) {
        counter += (int)strlen(messages[i]);
    }
    printf("%d %d\n", counter, argc);
    return counter > 0 ? 0 : 1;
}
"#;

#[test]
fn dyld_info_decodes_lazy_bindings_into_la_symbol_ptr() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();
    assert!(
        obj.dyld_info().is_some(),
        "{LS} should be an LC_DYLD_INFO_ONLY image"
    );

    let (encoding, fixups) = decode_image(&obj).unwrap();
    assert_eq!(encoding, "dyld_info");

    let la = obj
        .section("__DATA", "__la_symbol_ptr")
        .expect("no __la_symbol_ptr");
    let lazy: Vec<_> = fixups
        .iter()
        .filter(|f| {
            matches!(
                f,
                Fixup::Bind {
                    kind: BindKind::Lazy,
                    ..
                }
            )
        })
        .collect();

    // Structural, not literal: one lazy binding per slot, every one inside the
    // section. The count follows from the section size, so a relaid-out `/bin/ls`
    // still passes.
    let slots = la.size / 8;
    assert_eq!(
        lazy.len() as u64,
        slots,
        "expected one lazy bind per __la_symbol_ptr slot"
    );
    for f in &lazy {
        assert!(
            la.contains(f.address()),
            "lazy bind at {:#x} outside __la_symbol_ptr",
            f.address()
        );
    }
}

#[test]
fn bind_ordinals_name_a_real_load_command() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();
    let (_, fixups) = decode_image(&obj).unwrap();

    let deps: Vec<&str> = obj.dependencies().iter().map(|d| d.install_name).collect();
    assert!(!deps.is_empty());

    let mut two_level = 0;
    for f in &fixups {
        let Fixup::Bind { symbol, .. } = f else {
            continue;
        };
        match symbol.library {
            LibraryRef::InstallName(n) => {
                assert!(
                    deps.contains(&n),
                    "{n} is not one of this image's dependencies"
                );
                two_level += 1;
            }
            LibraryRef::UnknownOrdinal(o) => panic!("bind referenced unknown ordinal {o}"),
            _ => {}
        }
        // `dyld_stub_binder` is the one import a Mach-O spells without a
        // leading underscore, so the invariant is "unchanged from the file",
        // not "starts with an underscore".
        assert!(
            symbol.name.starts_with('_') || symbol.name == "dyld_stub_binder",
            "{} is not a name this image could have written",
            symbol.name
        );
    }
    assert!(
        two_level > 0,
        "a two-level image should have two-level bindings"
    );
}

#[test]
fn chained_fixups_decode_into_the_same_shape() {
    let Some(bytes) = common::system_binary(ECHO) else {
        skip!("{ECHO} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();
    let Some(_) = obj.chained_fixups() else {
        skip!("{ECHO} is not a chained-fixups image")
    };

    let (encoding, fixups) = decode_image(&obj).unwrap();
    assert_eq!(encoding, "chained");
    assert!(!fixups.is_empty());

    let base = obj.preferred_base().unwrap();
    let end = base + obj.vm_span().unwrap();
    for f in &fixups {
        assert!(
            (base..end).contains(&f.address()),
            "fixup at {:#x} outside the image",
            f.address()
        );
        // A chained rebase must carry its target: the packed word in the slot
        // is not a pointer, so `Rebase` would be silently wrong here.
        assert!(
            !matches!(f, Fixup::Rebase { .. }),
            "chained decoder emitted a stored-value rebase"
        );
    }
}

#[test]
fn arm64e_chained_pointers_decode_to_plain_targets() {
    // The arm64e slice of a system binary is the only easy source of a
    // PAC-formatted chain. Its signatures are dropped, so every rebase must
    // land inside the image and every bind must name an import.
    use machish::spec::arch::Arm64;
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let Ok(obj) = MachObject::parse_any(&bytes, Arm64) else {
        skip!("no arm64 slice")
    };
    if obj.chained_fixups().is_none() {
        skip!("arm64 slice does not use chained fixups");
    }

    let (name, fixups) = decode_image(&obj).expect("arm64e chains decode");
    assert_eq!(name, "chained");
    assert!(fixups.iter().any(Fixup::is_bind), "no binds decoded");
    let base = obj.preferred_base().unwrap();
    for f in &fixups {
        if let Fixup::RebaseTo { target, .. } = f {
            // No signature or tag bits survive into the target.
            assert!(target >> 48 == 0, "signature bits leaked: {target:#x}");
            assert!(*target >= base, "target below the image: {target:#x}");
        }
    }
}

#[test]
fn both_encodings_decode_to_the_same_stream() {
    let Some(chained) = common::build_x86_64("agree_chained", AGREE_SRC, &[]) else {
        skip!("cc -arch x86_64 unavailable")
    };
    let Some(opcodes) = common::build_x86_64("agree_opcodes", AGREE_SRC, &["-Wl,-no_fixup_chains"])
    else {
        skip!("cc -arch x86_64 unavailable")
    };

    let a = MachObject::parse_any(&chained, X86_64).unwrap();
    let b = MachObject::parse_any(&opcodes, X86_64).unwrap();
    assert!(a.chained_fixups().is_some(), "expected chained fixups");
    assert!(b.dyld_info().is_some(), "expected LC_DYLD_INFO_ONLY");

    let (_, fa) = decode_image(&a).unwrap();
    let (_, fb) = decode_image(&b).unwrap();

    // Compare by (symbol, library) rather than by address: the two linker
    // invocations lay the image out differently, so slot addresses legitimately
    // differ while the *set of imports* must not.
    let binds = |obj: &MachObject<'_, X86_64>, fixups: &[Fixup<'_>]| -> BTreeMap<String, String> {
        let _ = obj;
        fixups
            .iter()
            .filter_map(|f| match f {
                // `dyld_stub_binder` is the one legitimate difference between
                // the encodings, and excluding it is not weakening the test.
                // The opcode encoding needs it because `__stub_helper` calls
                // into dyld to resolve a lazy slot on first use; chained fixups
                // have no `__stub_helper` at all, so the import genuinely does
                // not exist in that image rather than being missed by us.
                Fixup::Bind { symbol, .. } if symbol.name != "dyld_stub_binder" => Some((
                    symbol.name.to_string(),
                    symbol
                        .library
                        .install_name()
                        .unwrap_or("<flat>")
                        .to_string(),
                )),
                _ => None,
            })
            .collect()
    };

    let ba = binds(&a, &fa);
    let bb = binds(&b, &fb);
    assert!(!ba.is_empty(), "no imports decoded from the chained image");
    assert_eq!(
        ba, bb,
        "the two encodings disagree about this image's imports"
    );
}

#[test]
fn a_backwards_add_addr_uleb_wraps_instead_of_erroring() {
    use machish::spec::constants as c;

    // One __DATA segment at 0x1000, so a segment offset is an address minus it.
    let mut cmds = Vec::new();
    let mut seg = vec![0u8; c::SEGMENT_COMMAND_64_SIZE];
    seg[0..4].copy_from_slice(&c::LC_SEGMENT_64.to_le_bytes());
    seg[4..8].copy_from_slice(&(c::SEGMENT_COMMAND_64_SIZE as u32).to_le_bytes());
    seg[8..14].copy_from_slice(b"__DATA");
    seg[24..32].copy_from_slice(&0x1000u64.to_le_bytes()); // vmaddr
    seg[32..40].copy_from_slice(&0x1000u64.to_le_bytes()); // vmsize
    cmds.extend_from_slice(&seg);

    // A bind stream: bind at offset 0x100, then step *back* 0x108 bytes and
    // bind again at offset 0x8.
    let back = (-0x108i64) as u64;
    let mut stream = vec![c::BIND_OPCODE_SET_DYLIB_ORDINAL_IMM | 1];
    stream.push(c::BIND_OPCODE_SET_SYMBOL_TRAILING_FLAGS_IMM);
    stream.extend_from_slice(b"_foo\0");
    stream.push(c::BIND_OPCODE_SET_TYPE_IMM | c::BIND_TYPE_POINTER);
    stream.push(c::BIND_OPCODE_SET_SEGMENT_AND_OFFSET_ULEB); // segment 0
    stream.extend_from_slice(&[0x80, 0x02]); // uleb 0x100
    stream.push(c::BIND_OPCODE_DO_BIND); // -> 0x1100, leaves offset 0x108
    stream.push(c::BIND_OPCODE_ADD_ADDR_ULEB);
    let mut v = back;
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        stream.push(if v == 0 { byte } else { byte | 0x80 });
        if v == 0 {
            break;
        }
    }
    stream.push(c::BIND_OPCODE_DO_BIND); // -> 0x1000
    stream.push(c::BIND_OPCODE_DONE);

    let bind_off = c::MACH_HEADER_64_SIZE + cmds.len() + 48;
    let mut info = vec![0u8; 48];
    info[0..4].copy_from_slice(&c::LC_DYLD_INFO_ONLY.to_le_bytes());
    info[4..8].copy_from_slice(&48u32.to_le_bytes());
    info[16..20].copy_from_slice(&(bind_off as u32).to_le_bytes());
    info[20..24].copy_from_slice(&(stream.len() as u32).to_le_bytes());
    cmds.extend_from_slice(&info);

    let mut image = Vec::new();
    image.extend_from_slice(&c::MH_MAGIC_64.to_le_bytes());
    image.extend_from_slice(&c::CPU_TYPE_X86_64.to_le_bytes());
    image.extend_from_slice(&3i32.to_le_bytes());
    image.extend_from_slice(&c::MH_EXECUTE.to_le_bytes());
    image.extend_from_slice(&2u32.to_le_bytes()); // ncmds
    image.extend_from_slice(&(cmds.len() as u32).to_le_bytes());
    image.extend_from_slice(&0u32.to_le_bytes()); // flags
    image.extend_from_slice(&0u32.to_le_bytes()); // reserved
    image.extend_from_slice(&cmds);
    assert_eq!(image.len(), bind_off);
    image.extend_from_slice(&stream);

    let obj = MachObject::parse(&image, X86_64).unwrap();
    let (_, fixups) = decode_image(&obj).unwrap();
    let addrs: Vec<u64> = fixups.iter().map(Fixup::address).collect();
    assert_eq!(
        addrs,
        vec![0x1100, 0x1000],
        "the backwards step did not land where dyld puts it"
    );
}

#[test]
fn every_lazy_binding_carries_a_stub() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();
    let stubs = StubMap::build(&obj).unwrap();
    let (_, fixups) = decode_image(&obj).unwrap();

    let stub_sect = obj
        .section("__TEXT", "__stubs")
        .expect("no __stubs section");
    assert_eq!(stub_sect.reserved2, 6, "x86-64 stubs are six bytes");

    let mut checked = 0;
    for f in &fixups {
        let Fixup::Bind {
            at,
            kind: BindKind::Lazy,
            ..
        } = f
        else {
            continue;
        };
        let stub = stubs
            .stub_for(*at)
            .unwrap_or_else(|| panic!("no stub for slot {at:#x}"));
        assert!(stub_sect.contains(stub), "stub {stub:#x} outside __stubs");
        assert_eq!(
            (stub - stub_sect.addr) % 6,
            0,
            "stub {stub:#x} is not on the six-byte stride"
        );
        checked += 1;
    }
    assert!(checked > 0, "no lazy bindings to check");
}

#[test]
fn stub_addresses_actually_jump_through_their_slots() {
    // The claim being tested: `stub_va` is the address whose `jmp` lands on
    // `slot_va`. Decoding the instruction proves the pairing; comparing counts
    // or ranges would only show the two tables are the same size.
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();
    let stubs = StubMap::build(&obj).unwrap();
    assert!(!stubs.is_empty());

    let mut verified = 0;
    for (slot, stub) in stubs.iter() {
        let off = obj.vmaddr_to_off(stub).expect("stub has no file bytes");
        let code = &obj.bytes()[off..off + 6];
        // `ff 25 <rel32>` is `jmp qword [rip + rel32]`.
        assert_eq!(
            &code[..2],
            &[0xff, 0x25],
            "stub at {stub:#x} is not an indirect jmp"
        );
        let rel = i32::from_le_bytes([code[2], code[3], code[4], code[5]]);
        let target = (stub + 6).wrapping_add(rel as i64 as u64);
        assert_eq!(
            target, slot,
            "stub {stub:#x} jumps through {target:#x}, but was paired with slot {slot:#x}"
        );
        verified += 1;
    }
    assert!(
        verified >= 80,
        "expected many stubs in {LS}, verified only {verified}"
    );
}

/// A provider that answers everything with a distinct, recognisable address, so
/// a slot's contents identify which symbol was written there.
struct Counting {
    inner: SyntheticDylib,
}

impl Counting {
    fn over(obj: &MachObject<'_, X86_64>, fixups: &[Fixup<'_>]) -> (Self, BTreeMap<String, u64>) {
        let mut lib = SyntheticDylib::new("counting");
        lib.flat(true);
        let mut expected = BTreeMap::new();
        let mut next = 0x7000_0000_0000u64;
        for f in fixups {
            let Fixup::Bind { symbol, .. } = f else {
                continue;
            };
            if expected.contains_key(symbol.name) {
                continue;
            }
            lib.define(
                symbol.name,
                Resolved::function(next).with_tag(expected.len() as u64),
            );
            expected.insert(symbol.name.to_string(), next);
            next += 0x1000;
        }
        let _ = obj;
        (Counting { inner: lib }, expected)
    }
}

impl SymbolProvider for Counting {
    fn provides(&self, install_name: &str) -> bool {
        self.inner.provides(install_name)
    }
    fn resolve(&self, sym: &machish::fixups::SymbolRef<'_>) -> Option<Resolved> {
        self.inner.resolve(sym)
    }
}

#[test]
fn binding_writes_the_expected_pointer_into_every_slot() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mapped = map_segments(&obj, &mut mem).unwrap();
    let (_, fixups) = decode_image(&obj).unwrap();
    let stubs = StubMap::build(&obj).unwrap();

    let (provider, expected) = Counting::over(&obj, &fixups);
    let providers: [&dyn SymbolProvider; 1] = [&provider];
    let binder = Binder {
        slide: mapped.slide,
        providers: &providers,
        self_exports: None,
        main_executable: None,
    };

    let outcome = binder.apply(&fixups, &mut mem, &stubs).unwrap();
    assert!(
        outcome.missing.is_empty(),
        "unexpectedly missing: {:?}",
        outcome.missing
    );
    assert_eq!(
        outcome.bindings.len(),
        fixups.iter().filter(|f| f.is_bind()).count()
    );

    // Slot by slot: read back what was written and check it is the address the
    // provider promised for that symbol.
    for b in &outcome.bindings {
        let mut buf = [0u8; 8];
        mem.read(b.slot_va, &mut buf).unwrap();
        let got = u64::from_le_bytes(buf);
        assert_eq!(
            got, expected[&b.symbol],
            "wrong pointer in slot for {}",
            b.symbol
        );
        assert_eq!(
            got, b.resolved,
            "the record disagrees with memory for {}",
            b.symbol
        );
    }
}

#[test]
fn every_binding_record_is_fully_populated() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mapped = map_segments(&obj, &mut mem).unwrap();
    let (_, fixups) = decode_image(&obj).unwrap();
    let stubs = StubMap::build(&obj).unwrap();
    let (provider, _) = Counting::over(&obj, &fixups);
    let providers: [&dyn SymbolProvider; 1] = [&provider];

    let outcome = Binder {
        slide: mapped.slide,
        providers: &providers,
        self_exports: None,
        main_executable: None,
    }
    .apply(&fixups, &mut mem, &stubs)
    .unwrap();

    let stub_sect = obj.section("__TEXT", "__stubs").unwrap();
    let mut lazy = 0;
    for b in &outcome.bindings {
        assert!(b.symbol.starts_with('_') || b.symbol == "dyld_stub_binder");
        assert!(b.is_satisfied());
        if b.kind == BindKind::Lazy {
            lazy += 1;
            let stub = b
                .stub_va
                .unwrap_or_else(|| panic!("lazy binding {} has no stub", b.symbol));
            assert!(stub_sect.contains(stub.wrapping_sub(mapped.slide)));
        }
    }
    assert!(lazy > 0);

    // The record set must be usable as a lookup key for interception, i.e. no
    // two bindings may claim the same slot.
    let mut seen = BTreeMap::new();
    for b in &outcome.bindings {
        assert!(
            seen.insert(b.slot_va, &b.symbol).is_none(),
            "duplicate slot {:#x}",
            b.slot_va
        );
    }
}

#[test]
fn an_unsatisfied_hard_import_is_reported_not_silently_zeroed() {
    let Some(bytes) = common::system_binary(LS) else {
        skip!("{LS} not present")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mapped = map_segments(&obj, &mut mem).unwrap();
    let (_, fixups) = decode_image(&obj).unwrap();

    // No providers at all: everything must land in `missing`, and every slot
    // must read back as zero rather than as leftover file content.
    let outcome = Binder::new(mapped.slide)
        .apply(&fixups, &mut mem, &StubMap::default())
        .unwrap();
    assert!(outcome.bindings.is_empty());
    assert_eq!(
        outcome.missing.len(),
        fixups.iter().filter(|f| f.is_bind()).count()
    );

    for m in &outcome.missing {
        let mut buf = [0u8; 8];
        mem.read(m.slot_va, &mut buf).unwrap();
        assert_eq!(
            u64::from_le_bytes(buf),
            0,
            "{} left a stale pointer behind",
            m.symbol
        );
    }
}

const COALESCED_SRC: &str = r#"
    struct Box { int v; Box(int x) : v(x) {} };
    extern "C" int make(int x) { Box *b = new Box(x); int v = b->v; delete b; return v; }
"#;

fn coalesced_fixture() -> Option<Vec<u8>> {
    common::build_x86_64(
        "weak_coalesce",
        COALESCED_SRC,
        &["-x", "c++", "-lc++", "-dynamiclib", "-Wl,-no_fixup_chains"],
    )
}

#[test]
fn a_weak_bind_is_a_flat_lookup_and_never_the_images_own_exports() {
    let Some(bytes) = coalesced_fixture() else {
        skip!("cc -arch x86_64 -x c++ unavailable")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();
    let (_, fixups) = decode_image(&obj).unwrap();

    let weak: Vec<&Fixup<'_>> = fixups
        .iter()
        .filter(|f| {
            matches!(
                f,
                Fixup::Bind {
                    kind: BindKind::WeakCoalesce,
                    ..
                }
            )
        })
        .collect();
    assert!(
        !weak.is_empty(),
        "the fixture must produce a weak_bind stream"
    );

    // The ordinal register is never set in this stream, so it reads back as
    // zero - which is `BIND_SPECIAL_DYLIB_SELF`. Taking that at face value made
    // every weak bind claim to be satisfied by the image's own exports.
    for f in &weak {
        let Fixup::Bind { symbol, .. } = f else {
            unreachable!()
        };
        assert_eq!(
            symbol.library,
            LibraryRef::WeakLookup,
            "{} in weak_bind must be a flat coalescing lookup",
            symbol.name
        );
    }
    assert!(
        weak.iter()
            .any(|f| matches!(f, Fixup::Bind { symbol, .. } if symbol.name == "__Znwm")),
        "the fixture must coalesce operator new"
    );
}

#[test]
fn an_unsatisfied_weak_bind_leaves_the_ordinary_binding_alone() {
    let Some(bytes) = coalesced_fixture() else {
        skip!("cc -arch x86_64 -x c++ unavailable")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mapped = map_segments(&obj, &mut mem).unwrap();
    let (_, fixups) = decode_image(&obj).unwrap();
    let stubs = StubMap::build(&obj).unwrap();

    // Two-level only, deliberately: this provider answers the ordinary bind of
    // `__Znwm` from `libc++` and has nothing to say to a flat weak lookup. That
    // is the shape a real host chain has, and the weak pass must not undo it.
    let mut libcxx = SyntheticDylib::new("/usr/lib/libc++.1.dylib");
    for f in &fixups {
        if let Fixup::Bind { symbol, kind, .. } = f
            && *kind != BindKind::WeakCoalesce
        {
            libcxx.define(symbol.name, Resolved::function(0xdead_0000));
        }
    }
    let providers: [&dyn SymbolProvider; 1] = [&libcxx];
    let outcome = Binder {
        slide: mapped.slide,
        providers: &providers,
        self_exports: None,
        main_executable: None,
    }
    .apply(&fixups, &mut mem, &stubs)
    .unwrap();

    // A weak lookup nobody answers is not an unsatisfied import; it is a symbol
    // nobody overrode, and reporting it makes a healthy image look broken.
    // (Other libraries' imports are genuinely missing here - this provider
    // stands for `libc++` alone - so the claim is about the coalesced ones.)
    assert!(
        !outcome.missing.iter().any(|m| m.symbol == "__Znwm"),
        "a weak lookup was reported as a missing import: {:?}",
        outcome.missing
    );

    let new = outcome
        .bindings
        .iter()
        .find(|b| b.symbol == "__Znwm")
        .expect("the fixture must import operator new");
    let mut buf = [0u8; 8];
    mem.read(new.slot_va, &mut buf).unwrap();
    assert_eq!(
        u64::from_le_bytes(buf),
        0xdead_0000,
        "the weak pass zeroed a slot the bind stream had already resolved"
    );
}

#[test]
fn a_synthetic_dylib_satisfies_a_built_fixtures_imports() {
    // The end-to-end shape of `selfish/tests/aarch64_shim.rs:58-63`: build a
    // binary, bind it against addresses we chose, assert the slots.
    let src = r#"
        #include <string.h>
        size_t f(const char *s) { return strlen(s); }
        int main(void) { return (int)f("hello"); }
    "#;
    let Some(bytes) = common::build_x86_64("bind_synthetic", src, &["-Wl,-no_fixup_chains"]) else {
        skip!("cc -arch x86_64 unavailable")
    };
    let obj = MachObject::parse_any(&bytes, X86_64).unwrap();

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mapped = map_segments(&obj, &mut mem).unwrap();
    let (_, fixups) = decode_image(&obj).unwrap();
    let stubs = StubMap::build(&obj).unwrap();

    let mut lib = SyntheticDylib::new("shims");
    lib.flat(true);
    for f in &fixups {
        if let Fixup::Bind { symbol, .. } = f {
            lib.define(symbol.name, Resolved::function(0xdead_0000).with_tag(7));
        }
    }
    let providers: [&dyn SymbolProvider; 1] = [&lib];
    let outcome = Binder {
        slide: mapped.slide,
        providers: &providers,
        self_exports: None,
        main_executable: None,
    }
    .apply(&fixups, &mut mem, &stubs)
    .unwrap();

    assert!(outcome.missing.is_empty());
    let strlen: Vec<&BindingRecord> = outcome
        .bindings
        .iter()
        .filter(|b| b.symbol == "_strlen")
        .collect();
    assert!(!strlen.is_empty(), "the fixture must import _strlen");
    for b in strlen {
        assert_eq!(b.resolved, 0xdead_0000);
        assert_eq!(b.tag, 7, "the provider's tag must survive into the record");
        let mut buf = [0u8; 8];
        mem.read(b.slot_va, &mut buf).unwrap();
        assert_eq!(u64::from_le_bytes(buf), 0xdead_0000);
    }
}

/// A pre-10.6 executable: no `LC_DYLD_INFO`, one indirect pointer slot, and
/// data bound and slid only through `LC_DYSYMTAB`'s relocation tables.
fn classic_image_with_relocations() -> Vec<u8> {
    use machish::spec::constants::{
        CPU_TYPE_X86_64, LC_DYSYMTAB, LC_LOAD_DYLIB, LC_SEGMENT_64, LC_SYMTAB, MH_EXECUTE,
        MH_MAGIC_64, MH_TWOLEVEL,
    };
    const TEXT: u64 = 0x1_0000_0000;
    const DATA: u64 = 0x1_0000_1000;

    fn segment(name: &str, vmaddr: u64, fileoff: u64, prot: i32, sections: &[[u8; 80]]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&LC_SEGMENT_64.to_le_bytes());
        v.extend_from_slice(&(72 + 80 * sections.len() as u32).to_le_bytes());
        let mut n = [0u8; 16];
        n[..name.len()].copy_from_slice(name.as_bytes());
        v.extend_from_slice(&n);
        for x in [vmaddr, 0x1000, fileoff, 0x1000] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(&prot.to_le_bytes());
        v.extend_from_slice(&prot.to_le_bytes());
        v.extend_from_slice(&(sections.len() as u32).to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes());
        for s in sections {
            v.extend_from_slice(s);
        }
        v
    }

    // `__DATA,__nl_symbol_ptr`: one slot, indirect entry 0.
    let mut nl = [0u8; 80];
    nl[..15].copy_from_slice(b"__nl_symbol_ptr");
    nl[16..22].copy_from_slice(b"__DATA");
    nl[32..40].copy_from_slice(&DATA.to_le_bytes());
    nl[40..48].copy_from_slice(&8u64.to_le_bytes());
    nl[48..52].copy_from_slice(&0x1000u32.to_le_bytes());
    nl[64..68].copy_from_slice(&6u32.to_le_bytes()); // S_NON_LAZY_SYMBOL_POINTERS

    let mut cmds = Vec::new();
    cmds.extend(segment("__TEXT", TEXT, 0, 5, &[]));
    cmds.extend(segment("__DATA", DATA, 0x1000, 3, &[nl]));
    let dylib = b"/usr/lib/libobjc.A.dylib\0\0\0\0\0\0\0\0";
    cmds.extend_from_slice(&LC_LOAD_DYLIB.to_le_bytes());
    cmds.extend_from_slice(&(24 + dylib.len() as u32).to_le_bytes());
    for x in [24u32, 0, 0, 0] {
        cmds.extend_from_slice(&x.to_le_bytes());
    }
    cmds.extend_from_slice(dylib);

    // __LINKEDIT-ish tail at 0x2000: symbols, strings, relocations, indirects.
    let symoff = 0x2000u32;
    let stroff = symoff + 2 * 16;
    let strings = b"\0_objc_msgSend_fixup\0_kCFAllocatorDefault\0";
    let reloff = stroff + strings.len() as u32;
    let locreloff = reloff + 8;
    let indirectoff = locreloff + 8;

    cmds.extend_from_slice(&LC_SYMTAB.to_le_bytes());
    cmds.extend_from_slice(&24u32.to_le_bytes());
    for x in [symoff, 2, stroff, strings.len() as u32] {
        cmds.extend_from_slice(&x.to_le_bytes());
    }
    cmds.extend_from_slice(&LC_DYSYMTAB.to_le_bytes());
    cmds.extend_from_slice(&80u32.to_le_bytes());
    let dysym = [
        0u32,
        0,
        0,
        0,
        0,
        2,
        0,
        0,
        0,
        0,
        0,
        0,
        indirectoff,
        1,
        reloff,
        1,
        locreloff,
        1,
    ];
    for x in dysym {
        cmds.extend_from_slice(&x.to_le_bytes());
    }

    let mut v = Vec::new();
    v.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
    v.extend_from_slice(&CPU_TYPE_X86_64.to_le_bytes());
    v.extend_from_slice(&3i32.to_le_bytes());
    v.extend_from_slice(&MH_EXECUTE.to_le_bytes());
    v.extend_from_slice(&5u32.to_le_bytes());
    v.extend_from_slice(&(cmds.len() as u32).to_le_bytes());
    v.extend_from_slice(&MH_TWOLEVEL.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend(cmds);
    v.resize(0x2000, 0);
    // An addend already in the bound slot, and a local pointer to slide.
    v[0x1010..0x1018].copy_from_slice(&4u64.to_le_bytes());
    v[0x1020..0x1028].copy_from_slice(&(TEXT + 0x80).to_le_bytes());

    for (strx, ordinal) in [(1u32, 1u16), (21, 1)] {
        v.extend_from_slice(&strx.to_le_bytes());
        v.push(0x01); // N_UNDF | N_EXT
        v.push(0);
        v.extend_from_slice(&(ordinal << 8).to_le_bytes());
        v.extend_from_slice(&0u64.to_le_bytes());
    }
    v.extend_from_slice(strings);
    // r_address is from the first writable segment; extern UNSIGNED quad.
    v.extend_from_slice(&0x10u32.to_le_bytes());
    v.extend_from_slice(&((3u32 << 25) | (1 << 27)).to_le_bytes());
    v.extend_from_slice(&0x20u32.to_le_bytes());
    v.extend_from_slice(&(1u32 | (3 << 25)).to_le_bytes());
    v.extend_from_slice(&1u32.to_le_bytes()); // indirect: symbol 1
    v
}

#[test]
fn classic_external_relocations_bind_and_local_ones_rebase() {
    let bytes = classic_image_with_relocations();
    let obj = MachObject::parse(&bytes, X86_64).unwrap();
    let (encoding, fixups) = decode_image(&obj).unwrap();
    assert_eq!(encoding, "classic");

    let binds: Vec<_> = fixups
        .iter()
        .filter_map(|f| match f {
            Fixup::Bind {
                at, symbol, addend, ..
            } => Some((*at, symbol.name, *addend, symbol.library)),
            _ => None,
        })
        .collect();
    assert!(binds.contains(&(
        0x1_0000_1000,
        "_kCFAllocatorDefault",
        0,
        LibraryRef::InstallName("/usr/lib/libobjc.A.dylib")
    )));
    assert!(binds.contains(&(
        0x1_0000_1010,
        "_objc_msgSend_fixup",
        4,
        LibraryRef::InstallName("/usr/lib/libobjc.A.dylib")
    )));
    assert!(fixups.contains(&Fixup::Rebase { at: 0x1_0000_1020 }));
}
