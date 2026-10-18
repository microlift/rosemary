use std::collections::BTreeSet;
use std::ffi::OsString;
use std::process::ExitCode;

use rosemary::{
    Classification, Engine, Execution, ImportPolicy, Native, Program, Vm, audit_call_targets,
    startup,
};

use clap::{Args, Parser, Subcommand};
use microlift::arch::x86_64::X86_64;
use microlift::core::lift_to_function;

use mimalloc::MiMalloc;

mod prefix_cli;
mod server_cli;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// Load and execute, with `argv[0]` the path as the guest will see it.
///
/// `path` is this host's - it is what gets opened - and `guest` is the spelling
/// the guest is given, which under a prefix is not the same string. See
/// `docs/prefixes.md`.
fn run(
    path: &str,
    guest: &str,
    args: Vec<OsString>,
    policy: ImportPolicy,
    forced: Option<Engine>,
) -> ExitCode {
    darwin::spawn::set_emulated(forced == Some(Engine::Emulated));
    let mut argv = vec![OsString::from(guest)];
    argv.extend(args);
    rosemary::stats::start();

    let engine = match choose(path, forced) {
        Ok(engine) => engine,
        Err(e) => {
            eprintln!("rosemary: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = prefix_cli::ensure_cxx_runtime(engine) {
        eprintln!("rosemary: {path}: {e}");
        return ExitCode::FAILURE;
    }
    prefix_cli::check_guest_arch(match engine {
        Engine::Native => prefix::pinned::host_arch(),
        Engine::Emulated => "x86_64",
    });

    let exec: &'static dyn Execution = match engine {
        // Deliberately leaked: the program's mappings must outlive `main`.
        Engine::Native => match Native::load(path, &argv) {
            Ok(native) => Box::leak(Box::new(native)),
            Err(e) => {
                eprintln!("rosemary: {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        Engine::Emulated => match emulated(path, &argv, policy) {
            Ok(vm) => vm,
            Err(e) => {
                eprintln!("rosemary: {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    rosemary::exec::publish(exec);
    // After loading, which settles the CPU type the guest is listed with, and
    // before its first instruction, so it is listed from the start.
    if darwin::libc_backend::using_builtin() {
        server_cli::join(guest, &argv);
    }

    match startup::run(exec) {
        // `exit` rather than returning the status. Returning goes back through
        // Rust's start-up shim, which tears this thread's TLS down *before*
        // calling `exit` - and the guest's `atexit` handlers run from inside
        // `exit` and re-enter the JIT. Compiling a region needs live
        // thread-locals (Cranelift's among them), so a handler whose code had
        // not yet been translated would panic instead of running.
        Ok(status) => std::process::exit(status),
        // A stop is a diagnostic, not a crash.
        Err(e) => {
            eprintln!("rosemary: {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The backend for `path`: what was asked for, or what the file's slices allow.
fn choose(path: &str, forced: Option<Engine>) -> Result<Engine, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    match forced {
        Some(engine) => match engine.refusal(&bytes) {
            Some(why) => Err(why),
            None => Ok(engine),
        },
        None => Engine::select(&bytes)
            .ok_or_else(|| "no slice this runtime can run or recompile".to_string()),
    }
}

/// The emulated backend, published for the rest of the process.
fn emulated(
    path: &str,
    argv: &[OsString],
    policy: ImportPolicy,
) -> Result<&'static Vm, Box<dyn std::error::Error>> {
    let program = Program::load_executable(path, &[])?;
    let vm = Vm::with_policy(program, path, argv, policy)?;
    // Deliberately leaked, and published for the rest of the process.
    let vm: &'static Vm = Box::leak(Box::new(vm));
    rosemary::thunk::publish(vm);

    // Objective-C registration happens inside `startup::run`, in dyld's order.
    // It is driven from here first only so that its refusals reach the user.
    for r in vm.register_objc()?.refusals() {
        eprintln!("rosemary: objc: {r:?}");
    }
    Ok(vm)
}

/// The program to open, for a path that may be spelled either way.
///
/// Usually a person typed a host path. But this runtime also re-executes
/// *itself* for a guest that spawned another Mach-O, and hands the child the
/// path the guest used - so `rosemary run /Applications/Foo.app/...` is an
/// ordinary thing to be asked, and only the prefix knows where that is.
///
/// The prefix's spelling wins when there is a file there, and the path as given
/// is the fallback. Host-first would let this host's own files shadow the
/// guest's: `/bin/sh` exists on nearly every Linux host, so a guest spawning
/// `/bin/sh` would be handed the host's ELF shell instead of the prefix's.
fn program_path(path: &str) -> String {
    let host = prefix::ns::host_path(path);
    match host.exists() {
        true => host.to_string_lossy().into_owned(),
        false => path.to_string(),
    }
}

/// How many nested `#!` interpreters are followed, as Darwin's kernel does.
const SHEBANG_DEPTH: usize = 4;

/// Resolve `path` through its `#!` lines to the Mach-O that really runs,
/// with the arguments that interpreter gets: `[argument] script args...`.
///
/// The interpreter is a guest path, so `#!/bin/sh` is the prefix's shell
/// and not this host's. A file with no `#!` line is returned unchanged.
fn shebang(path: &str, mut args: Vec<OsString>) -> Result<(String, Vec<OsString>), String> {
    use std::os::unix::ffi::OsStringExt;
    let mut path = path.to_owned();
    for _ in 0..SHEBANG_DEPTH {
        let Ok(on_disk) = std::ffi::CString::new(path.as_str()) else {
            break;
        };
        let Some((program, argument)) = darwin::spawn::interpreter(&on_disk) else {
            return Ok((path, args));
        };
        let program = program
            .into_string()
            .map_err(|_| "the #! interpreter's path is not valid UTF-8".to_string())?;
        let resolved = program_path(&program);
        if !std::path::Path::new(&resolved).is_file() {
            return Err(format!("bad interpreter: {program}: no such file"));
        }
        // The script keeps the spelling the guest would give it.
        let mut next: Vec<OsString> = argument
            .map(|a| OsString::from_vec(a.into_bytes()))
            .into_iter()
            .collect();
        next.push(OsString::from(guest_spelling(&path)));
        next.append(&mut args);
        args = next;
        path = resolved;
    }
    match darwin::spawn::interpreter(&std::ffi::CString::new(path.as_str()).unwrap_or_default()) {
        Some(_) => Err("too many levels of #! interpreters".to_string()),
        None => Ok((path, args)),
    }
}

/// A host path as the guest spells it, or the path itself when no mount
/// reaches it - which is every path in identity mode.
///
/// Made absolute but never resolved: a multi-call program picks its
/// personality from `argv[0]`, and a `.app` reached through a symlink finds
/// its data beside the link, as `_NSGetExecutablePath` reports it on a Mac.
fn guest_spelling(path: &str) -> String {
    let path = std::path::Path::new(path);
    let host = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    prefix::ns::to_guest(&host)
        .unwrap_or(host)
        .to_string_lossy()
        .into_owned()
}

/// On macOS a prefix needs the built-in libc and Objective-C runtime, and the
/// built-in Objective-C runtime makes re-executing from a bundle mirror
/// pointless. Explicit settings win.
#[cfg(target_os = "macos")]
fn imply_builtin_runtime() {
    let set_default = |key: &str, value: &str| {
        if std::env::var_os(key).is_none() {
            // SAFETY: before any thread has been started.
            unsafe { std::env::set_var(key, value) };
        }
    };
    let prefixed = std::env::var_os("ROSEMARY_PREFIX").is_some_and(|named| {
        !matches!(
            prefix::layout::setting_from(Some(named)),
            prefix::layout::Setting::Off
        )
    });
    if prefixed {
        set_default("ROSEMARY_LIBC", "builtin");
        set_default("ROSEMARY_OBJC", "builtin");
    }
    if std::env::var("ROSEMARY_OBJC").as_deref() == Ok("builtin") {
        set_default("ROSEMARY_BUNDLED", "1");
    }
}

/// How a guest is run; shared by `rosemary run` and the bare form.
#[derive(Args, Debug, Default)]
struct RunOptions {
    /// Turn every import into a VM exit instead of binding known ones directly
    #[arg(long)]
    slow: bool,
    /// Run the host-architecture slice directly, never through the lifter
    #[arg(long, conflicts_with = "emulated")]
    native: bool,
    /// Recompile the x86-64 slice through Microlift even when a native slice exists
    #[arg(long)]
    emulated: bool,
    /// Enable the built-in Objective-C runtime's fail-fast checks (needs ROSEMARY_OBJC=builtin)
    #[arg(long)]
    objc_debug: bool,
}

/// `rosemary [OPTIONS] <PATH> [ARGS]...`, the form without a subcommand.
#[derive(Args, Debug)]
struct Target {
    /// Mach-O executable or .app bundle
    path: Option<OsString>,
    /// Arguments for the guest, or flags for the analysis. Put `--` before any
    /// guest argument that is spelled like one of this tool's flags.
    #[arg(allow_hyphen_values = true)]
    args: Vec<OsString>,
    #[command(flatten)]
    run: RunOptions,
    /// Report what loading the program finds instead of running it: images,
    /// imports, ABI coverage, Objective-C metadata and one lifted region
    #[arg(long)]
    analyze: bool,
    /// With the analysis, also check every direct call target (implies --analyze)
    #[arg(long)]
    audit: bool,
    /// With the analysis, print the lifted IR (implies --analyze)
    #[arg(long)]
    ir: bool,
}

#[derive(Parser, Debug)]
#[command(
    name = "rosemary",
    version,
    about = "Run macOS applications across architectures and operating systems",
    args_conflicts_with_subcommands = true,
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    target: Target,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run a Mach-O executable or .app bundle
    Run {
        #[command(flatten)]
        options: RunOptions,
        /// The program, then its arguments, which are passed through untouched
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        program: Vec<OsString>,
    },
    /// Manage prefixes: create, info, verify, repair, reset-system, update, cache
    #[command(disable_help_flag = true)]
    Prefix {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Run or stop this prefix's process server
    #[command(disable_help_flag = true)]
    Server {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
}

fn main() -> ExitCode {
    // Parsed from `args_os`: a process argument is a byte string, and
    // `std::env::args` *panics* on one that is not UTF-8. Some apps spawn
    // with annotations copied out of `sysctl`, so the argument
    // that aborted this runtime before it had loaded anything was the guest's
    // own and perfectly legal.
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Prefix { args }) => prefix_cli::command(args.into_iter()),
        Some(Command::Server { args }) => server_cli::command(args.into_iter()),
        Some(Command::Run {
            options,
            mut program,
        }) => {
            let path = program.remove(0);
            launch(path, &options, program)
        }
        None => {
            let t = cli.target;
            let Some(path) = t.path else {
                // `arg_required_else_help` shows help for no arguments at all;
                // this is options with no program.
                eprintln!("rosemary: no program given; see `rosemary --help`");
                return ExitCode::FAILURE;
            };
            match t.analyze || t.audit || t.ir {
                true => analyze(path, t.ir, t.audit),
                false => launch(path, &t.run, t.args),
            }
        }
    }
}

/// Load `path` as a program and execute it.
fn launch(path: OsString, options: &RunOptions, args: Vec<OsString>) -> ExitCode {
    // Before anything can ask which prefix this is, including the paths
    // published below.
    prefix_cli::settle_prefix();
    #[cfg(target_os = "macos")]
    imply_builtin_runtime();
    // `--slow` turns every import into a VM exit; `--native`/`--emulated`
    // force a backend the file's own slices would not have chosen.
    let policy = match options.slow {
        true => ImportPolicy::AlwaysSlow,
        false => ImportPolicy::FastWhenKnown,
    };
    let engine = match (options.native, options.emulated) {
        (true, _) => Some(Engine::Native),
        (_, true) => Some(Engine::Emulated),
        _ => None,
    };
    let objc_debug = options.objc_debug;
    // Before anything can ask which prefix this is, including the paths
    // published below.
    prefix_cli::settle_prefix();
    #[cfg(target_os = "macos")]
    imply_builtin_runtime();
    // The guest's *arguments* are bytes; its own path is not. Refused by
    // name rather than converted, because every later use of it - the
    // bundle layout, `_NSGetExecutablePath`, the fault reports - is text.
    let Some(path) = path.to_str().map(str::to_owned) else {
        eprintln!("rosemary: the program's path is not valid UTF-8");
        return ExitCode::FAILURE;
    };
    if objc_debug {
        if std::env::var("ROSEMARY_OBJC").as_deref() == Ok("native") {
            eprintln!(
                "rosemary: --objc-debug requires the built-in Objective-C runtime; \
                 set ROSEMARY_OBJC=builtin"
            );
            return ExitCode::FAILURE;
        }
        if rosemary::objc::host::backend() != rosemary::objc::host::Backend::Builtin {
            eprintln!(
                "rosemary: --objc-debug requires the built-in Objective-C runtime; \
                 set ROSEMARY_OBJC=builtin"
            );
            return ExitCode::FAILURE;
        }
        objc::debug::enable();
    }
    // Before anything can ask: both say which program the guest is, and a
    // guest that finds its own resources through either must get the same
    // answer.
    // The argument may be a host path or one the guest spells; everything
    // below opens files, so it is resolved to this host's spelling first.
    let path = program_path(&path);
    let executable = match rosemary::bundle::executable(std::path::Path::new(&path)) {
        Ok(executable) => executable,
        Err(e) => {
            eprintln!("rosemary: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(path) = executable.to_str() else {
        eprintln!("rosemary: the bundle executable's path is not valid UTF-8");
        return ExitCode::FAILURE;
    };
    // A script runs as its `#!` interpreter does, with the script as an
    // argument, so every later use of the executable path names the
    // interpreter - as `_NSGetExecutablePath` does on a Mac.
    let (path, args) = match shebang(path, args) {
        Ok(resolved) => resolved,
        Err(e) => {
            eprintln!("rosemary: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let path = path.as_str();
    rosemary::shims::runtime::set_executable_path(std::path::Path::new(path));
    rosemary::frameworks::set_main_bundle(std::path::Path::new(path));
    if rosemary::bundle::enclosing(std::path::Path::new(path)).is_some() {
        darwin::env::launch_as_app();
    }
    // The same path as the guest spells it, for `argv[0]`.
    let guest = guest_spelling(path);
    // A Mac's `~/Library` subtree, which this host has not got and a guest
    // does not create for itself.
    #[cfg(not(target_os = "macos"))]
    foundation::paths::ensure_standard_directories();
    #[cfg(target_os = "macos")]
    if !rosemary::frameworks::using_builtin()
        && let Some(mirror) = rosemary::bundle::remirror(std::path::Path::new(path))
    {
        eprintln!("rosemary: re-executing from {}", mirror.display());
        let e = rosemary::bundle::reexec(&mirror);
        // Only reached if `execv` failed, in which case running anyway is
        // strictly better than not running.
        eprintln!("rosemary: could not re-execute from the bundle mirror: {e}");
    }
    run(path, &guest, args, policy, engine)
}

/// Load `path` without running it and report what was found.
fn analyze(path: OsString, show_ir: bool, audit_calls: bool) -> ExitCode {
    let Some(path) = path.to_str().map(str::to_owned) else {
        eprintln!("rosemary: the program's path is not valid UTF-8");
        return ExitCode::FAILURE;
    };
    // A bundle is analysed through the executable it would run.
    let path = match std::path::Path::new(&path).is_dir() {
        true => match rosemary::bundle::executable(std::path::Path::new(&path)) {
            Ok(executable) => executable.to_string_lossy().into_owned(),
            Err(e) => {
                eprintln!("rosemary: {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        false => path,
    };

    if let Err(e) = prefix_cli::ensure_cxx_runtime(Engine::Emulated) {
        eprintln!("rosemary: {path}: {e}");
        return ExitCode::FAILURE;
    }
    let program = match Program::load(&path, &[]) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rosemary: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (text_va, text) = &program.text;
    println!("{path}");
    for img in &program.report.images {
        println!(
            "  image {} at {:#x} (slide {:#x})",
            img.name, img.base, img.slide
        );
    }
    for line in rosemary::interpose::describe(&program.report) {
        println!("  {line}");
    }
    println!(
        "  __text {:#x} + {:#x}, {} function(s)",
        text_va,
        text.len(),
        program.functions.len()
    );
    println!(
        "  {} binding(s) -> {} registered address(es)",
        program.report.bindings().count(),
        program.imports.len()
    );
    let missing: usize = program.report.images.iter().map(|i| i.missing.len()).sum();
    if missing > 0 {
        println!("  {missing} import(s) unresolved (need shims)");
        // Which library each one names, because that is what decides where it
        // was looked for - a two-level reference consults only the providers
        // claiming that install name, and `<no library>` means a flat, self,
        // main-executable or weak reference, which consults all of them.
        for m in program
            .report
            .images
            .iter()
            .flat_map(|i| i.missing.iter())
            .take(40)
        {
            println!(
                "    unresolved {} from {}{}",
                m.symbol,
                m.library.as_deref().unwrap_or("<no library>"),
                if m.weak_import { " (weak)" } else { "" }
            );
        }
    }

    // What it would take to *call* each of those imports. An unknown symbol is
    // the only entry that matters.
    let classified = program.classify_imports();
    let count = |want: fn(&Classification) -> bool| classified.values().filter(|c| want(c)).count();
    println!(
        "  abi: {} fast, {} slow, {} refused, {} unknown",
        count(|c| matches!(c, Classification::Fast)),
        count(|c| matches!(c, Classification::Slow)),
        count(|c| matches!(c, Classification::Refused(_))),
        count(|c| matches!(c, Classification::Unknown)),
    );
    for (symbol, class) in &classified {
        match class {
            Classification::Unknown => println!("  abi: {symbol} has no signature"),
            Classification::Refused(reason) => println!("  abi: {symbol} refused: {reason}"),
            _ => {}
        }
    }

    // What the guest declares to the Objective-C runtime.
    for img in &program.report.images {
        let Some(objc) = &img.objc else { continue };
        let methods: usize = objc.implementations().count();
        println!(
            "  objc: {} class(es), {} categor(y/ies), {} protocol(s), {} method(s), {} selref(s)",
            objc.classes.len(),
            objc.categories.len(),
            objc.protocols.len(),
            methods,
            objc.selector_refs.len(),
        );
        for c in &objc.classes {
            println!(
                "  objc: class {} : {:#x}{}",
                c.ro.name,
                c.superclass,
                if c.ro.is_root() { " (root)" } else { "" }
            );
        }
        for c in &objc.categories {
            println!("  objc: category {} on {:#x}", c.name, c.class);
        }
        for r in &objc.refusals {
            println!("  objc: refused {r:?}");
        }
    }

    // One lift, on demand, of the region control would actually reach first.
    let config = program.lifter_config();
    match program.entry() {
        Some(entry) => match program.lift_region(entry) {
            Some((region, bytes)) => {
                let start = region.range.start;
                println!(
                    "  entry {entry:#x} in region {start:#x} + {:#x}",
                    bytes.len()
                );
                match lift_to_function::<X86_64>(&bytes, start, config) {
                    Ok(func) => {
                        println!("  lifted to {} IR block(s)", func.dfg.num_blocks());
                        if show_ir {
                            println!("{}", func.display());
                        }
                    }
                    // An unimplemented instruction is a gap in the *recompiler*,
                    // not evidence that linking or interception is wrong, so it
                    // is reported rather than treated as a load failure.
                    Err(e) => println!("  lift failed: {e}"),
                }
            }
            None => println!("  entry {entry:#x} is outside any known function"),
        },
        None => println!("  no entry point (a dylib or bundle)"),
    }

    if !audit_calls {
        return ExitCode::SUCCESS;
    }

    let registered: BTreeSet<u64> = program.import_addresses().collect();
    let audit = audit_call_targets(
        text,
        *text_va,
        &program.functions,
        program.text_range(),
        &registered,
    );
    println!(
        "\n  audit: {} of {} function(s) analysable ({} refused by the lifter)",
        audit.functions_lifted,
        program.functions.len(),
        audit.functions_skipped
    );
    println!(
        "  audit: {} direct call target(s), {} leaving __text, {} recognised as imports",
        audit.direct_calls, audit.out_of_region, audit.intercepted
    );
    for u in &audit.unresolved {
        println!(
            "  audit: call to {:#x} would be followed into as guest code",
            u.target
        );
    }

    if audit.is_clean() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
