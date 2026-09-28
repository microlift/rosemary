use std::collections::BTreeSet;
use std::ffi::OsString;
use std::process::ExitCode;

use rosetta3::{
    Classification, Engine, Execution, ImportPolicy, Native, Program, Vm, audit_call_targets,
    startup,
};

use microlift::arch::x86_64::X86_64;
use microlift::core::lift_to_function;

use mimalloc::MiMalloc;

mod prefix_cli;

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
    let mut argv = vec![OsString::from(guest)];
    argv.extend(args);
    rosetta3::stats::start();

    let engine = match choose(path, forced) {
        Ok(engine) => engine,
        Err(e) => {
            eprintln!("rosetta3: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    prefix_cli::check_guest_arch(match engine {
        Engine::Native => prefix::pinned::host_arch(),
        Engine::Emulated => "x86_64",
    });

    let exec: &'static dyn Execution = match engine {
        // Deliberately leaked: the program's mappings must outlive `main`.
        Engine::Native => match Native::load(path, &argv) {
            Ok(native) => Box::leak(Box::new(native)),
            Err(e) => {
                eprintln!("rosetta3: {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        Engine::Emulated => match emulated(path, &argv, policy) {
            Ok(vm) => vm,
            Err(e) => {
                eprintln!("rosetta3: {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    rosetta3::exec::publish(exec);

    match startup::run(exec) {
        // `exit` rather than returning the status. Returning goes back through
        // Rust's start-up shim, which tears this thread's TLS down *before*
        // calling `exit` - and the guest's `atexit` handlers run from inside
        // `exit` and re-enter the JIT. Compiling a region needs live
        // thread-locals (Cranelift's among them), so a handler whose code had
        // not yet been translated would panic instead of running.
        Ok(status) => {
            // Recorded for the `atexit` footer of a per-process log, which is
            // the only place a child's exit status is ever written down: the
            // harness sees the status of the browser process and nothing else.
            LOG_STATUS.store(status, std::sync::atomic::Ordering::Relaxed);
            std::process::exit(status)
        }
        // A stop is a diagnostic, not a crash.
        Err(e) => {
            eprintln!("rosetta3: {path}: {e}");
            LOG_STATUS.store(1, std::sync::atomic::Ordering::Relaxed);
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
    rosetta3::thunk::publish(vm);

    // Objective-C registration happens inside `startup::run`, in dyld's order.
    // It is driven from here first only so that its refusals reach the user.
    for r in vm.register_objc()?.refusals() {
        eprintln!("rosetta3: objc: {r:?}");
    }
    Ok(vm)
}

/// The program to open, for a path that may be spelled either way.
///
/// Usually a person typed a host path. But this runtime also re-executes
/// *itself* for a guest that spawned another Mach-O, and hands the child the
/// path the guest used - so `rosetta3 run /Applications/Foo.app/...` is an
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

/// `ROSETTA3_LOG_DIR`: where this process's log file goes, if it gets one.
const LOG_DIR_ENV: &str = "ROSETTA3_LOG_DIR";

/// Whether [`process_log`] took this process's stderr over, for [`log_footer`].
static LOG_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// This process's pid, for [`log_footer`].
static LOG_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// The status this process is about to exit with, or `-1` for a path that
/// never said - for [`log_footer`].
static LOG_STATUS: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// Point this process's whole stderr at a file of its own, named for this
/// process, when `ROSETTA3_LOG_DIR` names a directory.
///
/// **Off by default.** Unset, this returns before it does anything, and the
/// runtime's output is what it has always been: the argument parser below
/// prints the same usage text, and no line anywhere gains a prefix. Every
/// other way of telling two processes apart has to be threaded through
/// something; this needs no cooperation from any single line, which is the
/// point, because there is no choke point to thread it through. `rosetta3: ` is
/// several hundred `eprintln!`s across twenty-odd crates, and the guest writes
/// to the same descriptor with its own `write(2)`.
///
/// **A file, not a tag per line,** because the two problems are different. A
/// tag needs a per-line edit at every site; a file needs one `dup2`. And the
/// file also removes the *interleaving*, which a tag cannot: two processes
/// writing one descriptor produce a log in which
/// `Failed to initialize sandbox.rosetta3: fault:` is a single line, and in
/// the last eight milestone logs each of them has a `rosetta3: fault:` that
/// begins mid-line. Everything a single-threaded process wrote around one of
/// those is unattributable, which is most of the log.
///
/// **Every guest child is a fresh `rosetta3 run`,** so each of them re-enters
/// here and takes a file of its own. The descriptor is deliberately *not*
/// close-on-exec: the child inherits this process's file as its stderr and
/// replaces it as the first thing it does, which is why there is nothing left
/// to share. Closing it instead would leave the child with no stderr at all,
/// and Rust's `eprintln!` panics when it cannot write - a run that worked would
/// be lost to a logging detail.
fn process_log() {
    use std::os::fd::AsRawFd;

    let Ok(dir) = std::env::var(LOG_DIR_ENV) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    // Milliseconds since the epoch in the name, so a lexical sort of the files
    // is a sort by process *start* time - the only ordering a set of
    // independent processes has.
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let pid = std::process::id();
    let path = dir.join(format!("{started:013}-{pid}"));
    let file = match std::fs::File::options().write(true).create(true).truncate(true).open(&path)
    {
        Ok(file) => file,
        Err(e) => {
            // Said on the *real* stderr, which is still the real stderr here.
            eprintln!("rosetta3: log: cannot open {LOG_DIR_ENV} file {}: {e}", path.display());
            eprintln!("rosetta3: log:   stderr is unchanged, so this run's log will interleave again");
            return;
        }
    };
    // SAFETY: `file` is a descriptor this process owns and has just opened, and
    // 2 is this process's own stderr. `dup2` returns 2's previous value or -1,
    // and does not close anything it did not create.
    if unsafe { libc::dup2(file.as_raw_fd(), 2) } < 0 {
        eprintln!("rosetta3: log: cannot make {} this process's stderr", path.display());
        eprintln!("rosetta3: log:   stderr is unchanged, so this run's log will interleave again");
        return;
    }
    // `file` is dropped here, which closes the descriptor it owns. The one
    // `dup2` made is a separate descriptor onto the same open file, and it is
    // the one everything below - and the whole process - writes to.
    drop(file);

    LOG_PID.store(pid, std::sync::atomic::Ordering::Relaxed);
    LOG_ON.store(true, std::sync::atomic::Ordering::Relaxed);
    // Runs at the end of `exit`, after the guest's own handlers and this
    // runtime's statics. Fails silently, which is all a footer may do: a
    // logging failure must never be the reason a run dies.
    unsafe { libc::atexit(log_footer) };

    eprintln!("rosetta3: log: pid {pid}, and this process's whole stderr, because {LOG_DIR_ENV} is set");
    // The two measurements that make any line in a *shared* log attributable
    // even if this header is lost: a Linux tid is this pid, and the load
    // address of libc is a per-process identity, because ASLR moves it and the
    // fault report prints both a host pc and the file offset it maps to.
    eprintln!("rosetta3: log: pid {pid}, tid {}, on the main thread", std::process::id());
    match libc_base() {
        Some(base) => eprintln!(
            "rosetta3: log: pid {pid}, libc base {base:#x} - a fault report's `host pc` minus its \
             `file offset` is this number, and no other process in the run has it"
        ),
        None => eprintln!(
            "rosetta3: log: pid {pid}, libc base not readable (/proc/self/maps); a fault report's \
             `host pc` is still this process's alone"
        ),
    }
    // The guest image, which is what tells two helpers apart.
    let argv: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    eprintln!("rosetta3: log: pid {pid}, argv {}", argv.join(" "));
    eprintln!("rosetta3: log: pid {pid}, file {}", path.display());
}

/// The load address of this process's libc, or `None` where `/proc` says nothing.
///
/// A shared object is mapped at an address ASLR chose, so this is an identity
/// no two processes in a run share, and the fault report already prints both
/// halves of the subtraction: a `host pc` and the `file offset` that pc maps
/// to. The lowest mapping of the object is the ELF load bias.
fn libc_base() -> Option<u64> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    for line in maps.lines() {
        // `address perms offset dev inode path`, and the path is absent when
        // the mapping is anonymous.
        if !line.split_whitespace().nth(5).is_some_and(|p| p.contains("libc.so")) {
            continue;
        }
        let start = line.split_whitespace().next()?.split('-').next()?;
        return u64::from_str_radix(start, 16).ok();
    }
    None
}

/// Say, from a normal exit, that the process got to the end.
///
/// `abort()` does not run `atexit` handlers and neither does a core dump, which
/// is the whole value of this: a per-process log with the footer is a process
/// that finished, and one without it is a process that was killed - including
/// by the harness's own sweep, which is otherwise indistinguishable from a
/// guest that hung. The harness's sweep, incidentally, is the reason this is
/// worth having: it kills processes on purpose.
extern "C" fn log_footer() {
    use std::sync::atomic::Ordering::Relaxed;
    if !LOG_ON.load(Relaxed) {
        return;
    }
    // No allocation, no formatting, nothing that can panic: this runs inside
    // `exit` while the runtime is being torn down, and a panic in an `atexit`
    // handler aborts a run that had otherwise worked.
    let mut buf = [0u8; 96];
    let mut at = 0;
    at += write_bytes(&mut buf, at, b"rosetta3: log: pid ");
    at += write_number(&mut buf, at, u64::from(LOG_PID.load(Relaxed)));
    at += write_bytes(&mut buf, at, b" exiting normally, status ");
    let status = LOG_STATUS.load(Relaxed);
    if status < 0 {
        at += write_bytes(&mut buf, at, b"?");
    } else {
        at += write_number(&mut buf, at, status as u64);
    }
    at += write_bytes(
        &mut buf,
        at,
        b"; an abort or a core dump prints no footer\n",
    );
    // SAFETY: `write` on descriptor 2, which [`process_log`] made this
    // process's own log file. One call, no state, async-signal-safe.
    unsafe { libc::write(2, buf.as_ptr().cast(), at as libc::size_t) };
}

/// `bytes` into `buf` at `at`, stopping at the end of the buffer.
fn write_bytes(buf: &mut [u8], at: usize, bytes: &[u8]) -> usize {
    let end = (at + bytes.len()).min(buf.len());
    buf[at..end].copy_from_slice(&bytes[..end - at]);
    end - at
}

/// `value` in decimal into `buf` at `at`, stopping at the end of the buffer.
fn write_number(buf: &mut [u8], at: usize, mut value: u64) -> usize {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + u8::try_from(value % 10).unwrap_or(0);
        value /= 10;
        if value == 0 {
            break;
        }
    }
    write_bytes(buf, at, &digits[i..])
}

fn main() -> ExitCode {
    // Before anything can print, and before the guest has been started, because
    // this is the only point at which this process's output is still knowable:
    // afterwards a dozen guest children share one descriptor with it.
    process_log();

    // `args_os`, not `args`: a process argument is a byte string, and
    // `std::env::args` *panics* on one that is not UTF-8. Some apps spawn
    // with annotations copied out of `sysctl`, so the argument
    // that aborted this runtime before it had loaded anything was the guest's
    // own and perfectly legal.
    let mut args = std::env::args_os().skip(1);
    let Some(first) = args.next() else {
        eprintln!(
            "usage: rosetta3 run [--native|--emulated] [--objc-debug] <mach-o path> [args...]"
        );
        eprintln!(
            "       rosetta3 prefix <create|info|verify|repair|reset-system|update|cache> ..."
        );
        eprintln!("       rosetta3 <mach-o path> [--audit] [--ir]");
        return ExitCode::FAILURE;
    };
    if first == "prefix" {
        return prefix_cli::command(args);
    }
    if first == "run" {
        // Before anything can ask which prefix this is, including the paths
        // published below.
        prefix_cli::settle_prefix();
        // `--slow` turns every import into a VM exit; `--native`/`--emulated`
        // force a backend the file's own slices would not have chosen.
        let mut policy = ImportPolicy::FastWhenKnown;
        let mut engine = None;
        let mut objc_debug = false;
        let mut path = None;
        for arg in args.by_ref() {
            match arg.to_str().unwrap_or_default() {
                "--slow" if path.is_none() => policy = ImportPolicy::AlwaysSlow,
                "--native" if path.is_none() => engine = Some(Engine::Native),
                "--emulated" if path.is_none() => engine = Some(Engine::Emulated),
                "--objc-debug" if path.is_none() => objc_debug = true,
                _ => {
                    path = Some(arg);
                    break;
                }
            }
        }
        let Some(path) = path else {
            eprintln!(
                "usage: rosetta3 run [--slow] [--native|--emulated] [--objc-debug] \
                 <mach-o path> [args...]"
            );
            return ExitCode::FAILURE;
        };
        // The guest's *arguments* are bytes; its own path is not. Refused by
        // name rather than converted, because every later use of it - the
        // bundle layout, `_NSGetExecutablePath`, the fault reports - is text.
        let Some(path) = path.to_str().map(str::to_owned) else {
            eprintln!("rosetta3: the program's path is not valid UTF-8");
            return ExitCode::FAILURE;
        };
        if objc_debug {
            if std::env::var("ROSETTA3_OBJC").as_deref() == Ok("native") {
                eprintln!(
                    "rosetta3: --objc-debug requires the built-in Objective-C runtime; \
                     set ROSETTA3_OBJC=builtin"
                );
                return ExitCode::FAILURE;
            }
            if rosetta3::objc::host::backend() != rosetta3::objc::host::Backend::Builtin {
                eprintln!(
                    "rosetta3: --objc-debug requires the built-in Objective-C runtime; \
                     set ROSETTA3_OBJC=builtin"
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
        rosetta3::shims::runtime::set_executable_path(std::path::Path::new(&path));
        rosetta3::frameworks::set_main_bundle(std::path::Path::new(&path));
        // The same path as the guest spells it, for `argv[0]`.
        let guest = guest_spelling(&path);
        // A Mac's `~/Library` subtree, which this host has not got and a guest
        // does not create for itself.
        #[cfg(not(target_os = "macos"))]
        foundation::paths::ensure_standard_directories();
        #[cfg(target_os = "macos")]
        if !rosetta3::frameworks::using_builtin()
            && let Some(mirror) = rosetta3::bundle::remirror(std::path::Path::new(&path))
        {
            eprintln!("rosetta3: re-executing from {}", mirror.display());
            let e = rosetta3::bundle::reexec(&mirror);
            // Only reached if `execv` failed, in which case running anyway is
            // strictly better than not running.
            eprintln!("rosetta3: could not re-execute from the bundle mirror: {e}");
        }
        return run(&path, &guest, args.collect(), policy, engine);
    }
    let Some(path) = first.to_str().map(str::to_owned) else {
        eprintln!("rosetta3: the program's path is not valid UTF-8");
        return ExitCode::FAILURE;
    };
    let rest: Vec<String> = args.map(|a| a.to_string_lossy().into_owned()).collect();
    let show_ir = rest.iter().any(|a| a == "--ir");
    let run_audit = rest.iter().any(|a| a == "--audit");

    let program = match Program::load(&path, &[]) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("rosetta3: {path}: {e}");
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
    for line in rosetta3::interpose::describe(&program.report) {
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

    if !run_audit {
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
