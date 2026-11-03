#![cfg(all(feature = "std", feature = "arch-x86_64"))]

mod common;

use std::path::PathBuf;
use std::process::Command;

use machish::loader::{LoadPolicy, Loader};
use machish::memory::{AddressSpace, BufferSpace};
use machish::namespace::FsDylibSource;
use machish::resolve::{NativeDylibProvider, SymbolProvider, SyntheticDylib, eager_binding_shims};
use machish::spec::arch::X86_64;

const MAIN_SRC: &str = r#"
#include <stdio.h>
extern int mathish_add(int, int);
extern int mathish_scale;
int main(void) {
    printf("%d\n", mathish_add(2, 3) * mathish_scale);
    return 0;
}
"#;

const LIB_SRC: &str = r#"
int mathish_scale = 7;
int mathish_add(int a, int b) { return a + b; }
"#;

/// Build `main` + `@rpath/libmath.dylib` in a private directory, returning
/// `(main path, main bytes)`. `None` if the toolchain cannot do it.
fn build_program(name: &str) -> Option<(PathBuf, Vec<u8>)> {
    if !common::can_build_x86_64() {
        return None;
    }
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    std::fs::create_dir_all(&dir).ok()?;

    let lib_src = dir.join("math.c");
    let lib_out = dir.join("libmath.dylib");
    std::fs::write(&lib_src, LIB_SRC).ok()?;
    let ok = Command::new("cc")
        .args([
            "-arch",
            "x86_64",
            "-dynamiclib",
            "-install_name",
            "@rpath/libmath.dylib",
            "-o",
        ])
        .arg(&lib_out)
        .arg(&lib_src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }

    let main_src = dir.join("main.c");
    let main_out = dir.join("main");
    std::fs::write(&main_src, MAIN_SRC).ok()?;
    let ok = Command::new("cc")
        .args(["-arch", "x86_64", "-Wl,-rpath,@loader_path", "-o"])
        .arg(&main_out)
        .arg(&main_src)
        .arg(&lib_out)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }

    let bytes = std::fs::read(&main_out).ok()?;
    Some((main_out, bytes))
}

#[test]
fn a_bundled_dylib_satisfies_the_executables_import() {
    let Some((path, bytes)) = build_program("loader_rpath") else {
        skip!("cc -arch x86_64 unavailable")
    };

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mut source = FsDylibSource::new();
    let shims = eager_binding_shims();
    let native = NativeDylibProvider::system();
    let providers: [&dyn SymbolProvider; 2] = [&shims, &native];

    let report = Loader::new(X86_64)
        .link_with(
            &bytes,
            path.to_str().unwrap(),
            &mut mem,
            &mut source,
            &providers,
        )
        .unwrap();

    // Two images: the executable and the dylib `@rpath` found next to it.
    assert_eq!(
        report.images.len(),
        2,
        "expected main + libmath, got {:?}",
        report
            .images
            .iter()
            .map(|i| i.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(report.images[1].name.contains("libmath"));

    // `mathish_add` must be bound, and bound to an address *inside libmath*,
    // not to anything the host provider dredged up.
    let add = report
        .bindings()
        .find(|b| b.symbol == "_mathish_add")
        .expect("_mathish_add was never bound");
    let lib = &report.images[1];
    let lib_end = lib
        .segments
        .iter()
        .map(|s| s.vmaddr + s.vmsize)
        .max()
        .unwrap();
    assert!(
        (lib.base..lib_end).contains(&add.resolved),
        "_mathish_add resolved to {:#x}, outside libmath [{:#x}, {:#x})",
        add.resolved,
        lib.base,
        lib_end
    );

    // And the data export too, which goes through a different slot kind.
    let scale = report
        .bindings()
        .find(|b| b.symbol == "_mathish_scale")
        .expect("_mathish_scale was never bound");
    assert!((lib.base..lib_end).contains(&scale.resolved));

    // Nothing left unresolved anywhere.
    report
        .require_all_resolved()
        .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn system_libraries_are_declined_by_the_filesystem_source_and_answered_natively() {
    let Some((path, bytes)) = build_program("loader_system") else {
        skip!("cc -arch x86_64 unavailable")
    };

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mut source = FsDylibSource::new();
    let shims = eager_binding_shims();
    let native = NativeDylibProvider::system();
    let providers: [&dyn SymbolProvider; 2] = [&shims, &native];

    let report = Loader::new(X86_64)
        .link_with(
            &bytes,
            path.to_str().unwrap(),
            &mut mem,
            &mut source,
            &providers,
        )
        .unwrap();

    // libSystem is a dependency, and it is *not* among the loaded images -
    // it was declined, exactly as intended, and satisfied symbol by symbol.
    assert!(
        !report.images.iter().any(|i| i.name.contains("libSystem")),
        "/usr/lib should never be mapped from disk"
    );
    let printf = report
        .bindings()
        .find(|b| b.symbol == "_printf")
        .expect("no _printf binding");
    assert_eq!(
        printf.library.as_deref(),
        Some("/usr/lib/libSystem.B.dylib")
    );
    assert!(
        printf.resolved != 0,
        "_printf should have resolved natively"
    );

    // A host address, not a guest one: the point of the whole design.
    let main = &report.images[0];
    let main_end = main
        .segments
        .iter()
        .map(|s| s.vmaddr + s.vmsize)
        .max()
        .unwrap();
    assert!(
        !(main.base..main_end).contains(&printf.resolved),
        "_printf resolved inside the guest image"
    );
}

#[test]
fn the_dylib_is_actually_mapped_and_its_code_readable() {
    let Some((path, bytes)) = build_program("loader_mapped") else {
        skip!("cc -arch x86_64 unavailable")
    };

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mut source = FsDylibSource::new();
    let native = NativeDylibProvider::system();
    let shims = eager_binding_shims();
    let providers: [&dyn SymbolProvider; 2] = [&shims, &native];

    let report = Loader::new(X86_64)
        .link_with(
            &bytes,
            path.to_str().unwrap(),
            &mut mem,
            &mut source,
            &providers,
        )
        .unwrap();

    // The binding says `_mathish_add` lives at this address; the mapped bytes
    // there must be the function the compiler emitted, not zeroes. Reading a
    // nonzero first instruction is enough to distinguish "mapped" from
    // "reserved and never written".
    let add = report
        .bindings()
        .find(|b| b.symbol == "_mathish_add")
        .unwrap();
    let mut code = [0u8; 8];
    mem.read(add.resolved, &mut code).unwrap();
    assert_ne!(code, [0u8; 8], "libmath's __TEXT was never copied in");

    // The two images must not overlap.
    let a = &report.images[0];
    let b = &report.images[1];
    let a_end = a
        .segments
        .iter()
        .map(|s| s.vmaddr + s.vmsize)
        .max()
        .unwrap();
    let b_end = b
        .segments
        .iter()
        .map(|s| s.vmaddr + s.vmsize)
        .max()
        .unwrap();
    assert!(
        a_end <= b.base || b_end <= a.base,
        "the two images overlap in the address space"
    );
}

#[test]
fn dependency_loading_can_be_turned_off() {
    let Some((path, bytes)) = build_program("loader_nodeps") else {
        skip!("cc -arch x86_64 unavailable")
    };

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let policy = LoadPolicy {
        load_dependencies: false,
        ..LoadPolicy::default()
    };
    let native = NativeDylibProvider::system();
    let shims = eager_binding_shims();
    let providers: [&dyn SymbolProvider; 2] = [&shims, &native];

    let report = Loader::with_policy(X86_64, policy)
        .link(&bytes, path.to_str().unwrap(), &mut mem, &providers)
        .unwrap();

    assert_eq!(report.images.len(), 1);
    // `_mathish_add` has nowhere to come from now, and must be reported as
    // missing rather than quietly bound to something.
    assert!(
        report.images[0]
            .hard_missing()
            .any(|m| m.symbol == "_mathish_add"),
        "_mathish_add should be unresolvable without libmath"
    );
    assert!(report.require_all_resolved().is_err());
}

#[test]
fn a_strict_policy_fails_the_link_on_a_missing_symbol() {
    let Some((path, bytes)) = build_program("loader_strict") else {
        skip!("cc -arch x86_64 unavailable")
    };

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let policy = LoadPolicy {
        load_dependencies: false,
        require_all_symbols: true,
        ..LoadPolicy::default()
    };
    let providers: [&dyn SymbolProvider; 0] = [];

    let err = Loader::with_policy(X86_64, policy)
        .link(&bytes, path.to_str().unwrap(), &mut mem, &providers)
        .unwrap_err();
    // The message names one symbol and counts the rest, rather than stopping at
    // the first one decoded.
    let machish::Error::UnresolvedSymbol(msg) = err else {
        panic!("expected UnresolvedSymbol, got {err}")
    };
    assert!(
        msg.contains("more"),
        "expected a summary of several missing symbols, got {msg:?}"
    );
}

#[test]
fn a_shim_can_take_priority_over_a_bundled_dylib() {
    // Provider order is the mechanism, and the loaded-image namespace comes
    // first by design - so overriding a bundled export needs the *namespace*
    // not to contain it. This checks the complementary case: a symbol the
    // bundled dylib does not export still reaches the caller's shims.
    let Some((path, bytes)) = build_program("loader_shim") else {
        skip!("cc -arch x86_64 unavailable")
    };

    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mut source = FsDylibSource::new();
    let mut shim = SyntheticDylib::new("shims");
    shim.flat(true)
        .define_fn("_printf", 0x1234_5678 as *const ());
    let shims = eager_binding_shims();
    let native = NativeDylibProvider::system();
    let providers: [&dyn SymbolProvider; 3] = [&shim, &shims, &native];

    let report = Loader::new(X86_64)
        .link_with(
            &bytes,
            path.to_str().unwrap(),
            &mut mem,
            &mut source,
            &providers,
        )
        .unwrap();

    let printf = report.bindings().find(|b| b.symbol == "_printf").unwrap();
    assert_eq!(
        printf.resolved, 0x1234_5678,
        "the shim should have won over the host"
    );
}

/// A bundle whose only load command is `LC_LOAD_DYLIB install_name`.
fn bundle_depending_on(install_name: &str) -> Vec<u8> {
    use machish::spec::constants::{
        CPU_TYPE_X86_64, LC_LOAD_DYLIB, MACH_HEADER_64_SIZE, MH_BUNDLE, MH_MAGIC_64,
    };
    // `dylib_command`: cmd, cmdsize, name offset, timestamp, two versions.
    let mut cmd = Vec::new();
    cmd.extend_from_slice(&LC_LOAD_DYLIB.to_le_bytes());
    cmd.extend_from_slice(&0u32.to_le_bytes());
    cmd.extend_from_slice(&24u32.to_le_bytes());
    cmd.resize(24, 0);
    cmd.extend_from_slice(install_name.as_bytes());
    cmd.push(0);
    cmd.resize(cmd.len().next_multiple_of(8), 0);
    let size = cmd.len() as u32;
    cmd[4..8].copy_from_slice(&size.to_le_bytes());

    let mut v = Vec::new();
    for word in [
        MH_MAGIC_64,
        CPU_TYPE_X86_64 as u32,
        3,
        MH_BUNDLE,
        1,
        size,
        0,
        0,
    ] {
        v.extend_from_slice(&word.to_le_bytes());
    }
    assert_eq!(v.len(), MACH_HEADER_64_SIZE);
    v.extend_from_slice(&cmd);
    v
}

/// Remembers every candidate it was asked for, and supplies nothing.
#[derive(Default)]
struct Recording(Vec<String>);

impl machish::namespace::DylibSource for Recording {
    fn fetch(
        &mut self,
        request: &machish::namespace::DylibRequest<'_>,
    ) -> machish::Result<Option<machish::namespace::FetchedDylib>> {
        self.0.extend(request.candidates.iter().cloned());
        Ok(None)
    }
}

/// A plugin loaded after startup names the program's directory with
/// `@executable_path`, not its own: Python extension modules in a subdirectory
/// find the program's bundled SDL that way.
#[test]
fn a_loaded_bundle_expands_executable_path_against_the_program() {
    let bytes = bundle_depending_on("@executable_path/libSDL2.dylib");
    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mut source = Recording::default();
    let mut loader = Loader::new(X86_64);
    loader.executable_path("/app/MacOS/game".to_string());
    // The link itself may refuse a bundle with no segments; the request it
    // made on the way is what is under test.
    let _ = loader.link_with(&bytes, "/app/MacOS/Lib/mod.so", &mut mem, &mut source, &[]);
    assert_eq!(source.0, ["/app/MacOS/libSDL2.dylib"]);
}

/// A stand-in for libSystem is not fetched, but the real image it re-exports
/// is - and only once, however many images depend on the umbrella.
#[test]
fn a_stand_ins_reexport_is_fetched_in_its_place() {
    let bytes = bundle_depending_on("/usr/lib/libSystem.B.dylib");
    let mut mem = BufferSpace::new(0x1_0000_0000);
    let mut source = Recording::default();
    let mut system = SyntheticDylib::new("/usr/lib/libSystem.B.dylib");
    system.reexport("/usr/lib/libcopyfile.dylib");
    let providers: [&dyn SymbolProvider; 1] = [&system];
    let _ = Loader::new(X86_64).link_with(
        &bytes,
        "/app/plugin.bundle",
        &mut mem,
        &mut source,
        &providers,
    );
    assert_eq!(source.0, ["/usr/lib/libcopyfile.dylib"]);
}
