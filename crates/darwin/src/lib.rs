#![allow(
    clippy::assertions_on_constants,
    clippy::bool_assert_comparison,
    clippy::explicit_counter_loop,
    clippy::identity_op,
    clippy::box_default,
    clippy::chunks_exact_to_as_chunks,
    clippy::doc_lazy_continuation,
    clippy::len_without_is_empty,
    clippy::manual_contains,
    clippy::manual_slice_size_calculation,
    clippy::missing_const_for_thread_local,
    clippy::missing_safety_doc,
    clippy::needless_return,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::question_mark,
    clippy::type_complexity,
    clippy::unnecessary_cast,
    clippy::unnecessary_lazy_evaluations,
    clippy::useless_vec
)]

use std::ffi::CString;

pub mod accounts;
#[cfg(not(target_os = "macos"))]
pub mod acl;
mod atomics;
pub mod attrlist;
pub mod audit;
pub mod backtrace;
pub mod bsd;
pub mod callback;
pub mod clock;
pub mod commoncrypto;
#[cfg(not(target_os = "macos"))]
pub mod compat;
pub mod copyfile;
pub mod credentials;
pub mod data;
pub mod dirent;
pub mod dispatch;
pub mod divergent;
pub mod env;
pub mod errno;
pub mod exe;
pub mod exit;
pub mod fcntl;
pub mod fmtcheck;
pub mod fortify;
pub mod fs;
pub mod fts;
#[cfg(target_os = "linux")]
pub mod getopt;
pub mod glob;
pub mod heap;
pub mod humanize;
pub mod icu;
pub mod idle;
pub mod images;
pub mod ioctl;
pub mod kqueue;
#[cfg(not(target_os = "macos"))]
pub mod langinfo;
pub mod libc_backend;
pub mod libproc;
pub mod locale;
pub mod mach;
pub mod malloc;
pub mod math;
pub mod modes;
pub mod net;
mod oslog;
pub mod pam;
pub mod passthrough;
pub mod passwd;
pub mod poll;
pub mod printf_l;
pub mod process;
pub mod progname;
pub mod pthread;
pub mod random;
pub mod regex;
mod resolv;
pub mod rlimit;
pub mod sched;
pub mod select;
pub mod shm;
pub mod signal;
pub mod sip;
pub mod sockpath;
pub mod spawn;
pub mod stackprobe;
pub mod stat;
pub mod stdio;
pub mod stdlib;
pub mod strict;
pub mod string;
pub mod strtofp;
pub mod sysctl;
pub mod sysdir;
pub mod sysinfo;
#[cfg(not(target_os = "macos"))]
pub mod termios;
pub mod tls;
pub mod translation;
pub mod ulock;
mod unfair_lock;
#[cfg(not(target_os = "macos"))]
pub mod utmpx;
pub mod uuid;
#[cfg(not(target_os = "macos"))]
pub mod vis;
pub mod vmmap;
#[cfg(not(target_os = "macos"))]
pub mod wait;
mod wait_word;
pub mod xattr;
pub mod xlocale;

/// The environment a guest's `main` receives as `envp`, as `NAME=VALUE`.
///
/// Builtin libc uses [`env`]'s block, which is the guest's own. Native libc
/// shares this process's environment.
pub fn guest_environment() -> Vec<CString> {
    if libc_backend::using_builtin() {
        return env::entries();
    }
    use std::os::unix::ffi::OsStrExt;
    // Bytes, not text: a value is an arbitrary byte string.
    std::env::vars_os()
        .filter_map(|(k, v)| CString::new([k.as_bytes(), b"=", v.as_bytes()].concat()).ok())
        .collect()
}

/// The value of `name` in the guest's environment block, which is not the
/// host's: see [`guest_environment`].
pub fn guest_env_var(name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    guest_environment().iter().find_map(|entry| {
        let entry = entry.to_string_lossy();
        entry.strip_prefix(&prefix).map(str::to_string)
    })
}

/// Strip the leading underscore a Mach-O symbol carries.
pub fn c_name(symbol: &str) -> &str {
    symbol.strip_prefix('_').unwrap_or(symbol)
}

/// [`c_name`], with a `$` variant suffix folded away as well.
///
/// `_close$NOCANCEL`, `_fopen$DARWIN_EXTSN`, `_fts_open$INODE64`: the suffix is a
/// promise about the *base* symbol's behaviour, not a different function, so the
/// base name is the one to ask every list about.
///
/// `machish::resolve::SyntheticDylib` folds the same suffix for the definitions
/// this crate publishes. This is the other half: a suffixed name that would
/// otherwise be handed to `dlsym` verbatim, which no host library exports, and
/// which no entry of [`divergent`] would match either - so a divergent function
/// spelled with a suffix would be passed straight through.
pub fn base_name(symbol: &str) -> &str {
    let name = c_name(symbol);
    match name.split_once('$') {
        Some((base, variant)) if !base.is_empty() && !variant.is_empty() => base,
        _ => name,
    }
}

/// `dlsym(RTLD_DEFAULT, name)` - this crate's own lookup against the host.
///
/// Not the answer a guest import gets: that goes through [`symbol`], which consults
/// this crate's entries first and refuses a divergent name outright.
pub fn host_symbol(name: &str) -> Option<u64> {
    load_host_libraries();
    let c = CString::new(name).ok()?;
    // SAFETY: `RTLD_DEFAULT` is a valid pseudo-handle and `c` outlives the call.
    // `dlsym` returns null for a name it does not know, which is checked.
    let found = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c.as_ptr()) };
    (!found.is_null()).then(|| found as usize as u64)
}

/// Make sure the libraries a guest's imports come out of are loaded before
/// anything asks for a symbol.
///
/// On Darwin `cos` is in libSystem, which the loader has already. Off it, it
/// lives in a library that is loaded only if something linked against it - and
/// this runtime links neither - so `dlsym(RTLD_DEFAULT, "cos")` is null and a
/// guest's whole maths library goes unresolved. One `dlopen` fixes every name
/// at once.
///
/// GL itself is deliberately **not** opened here. Its entry points have to come
/// out of the library SDL made the context against, which is not necessarily
/// this loader's `libGL.so.1`, and a global one would answer every `gl` name
/// first. See `rosemary::opengl`.
fn load_host_libraries() {
    #[cfg(not(target_os = "macos"))]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            for soname in [c"libm.so.6"] {
                // SAFETY: a soname the dynamic loader resolves, opened globally
                // so its exports join the default search, and never closed.
                unsafe {
                    libc::dlopen(soname.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL);
                }
            }
        });
    }
}

/// The address a guest asking for `symbol` should be given, or `None`.
///
/// `None` has two causes that a caller may want to tell apart - the name is
/// [`divergent`], or nothing anywhere defines it - and [`refusal`] says which.
pub fn symbol(symbol: &str) -> Option<u64> {
    let name = base_name(symbol);
    if let Some(addr) = implemented(name) {
        return Some(addr);
    }
    if divergent::contains(name)
        || strict::enabled()
        || !host_fallback_allowed(name)
        || stdio_refuses(name)
    {
        return None;
    }
    host_symbol(name)
}

/// A stdio name the builtin libc does not implement. The host's would be
/// handed one of [`stdio`]'s streams, which it cannot read.
fn stdio_refuses(name: &str) -> bool {
    libc_backend::using_builtin() && stdio::REFUSED.contains(&name)
}

/// [`symbol`] without the [`strict`] refusal, for a shim naming what it forwards to.
///
/// A shim that calls the host's `printf` is the explicit implementation the strict
/// mode asks for, not the fallback it withdraws - so it keeps its answer.
pub fn shim_symbol(symbol: &str) -> Option<u64> {
    let name = base_name(symbol);
    if let Some(addr) = implemented(name) {
        return Some(addr);
    }
    if divergent::contains(name) || !host_fallback_allowed(name) || stdio_refuses(name) {
        return None;
    }
    host_symbol(name)
}

/// Why [`symbol`] declined `symbol`, as a sentence.
pub fn refusal(symbol: &str) -> String {
    let name = base_name(symbol);
    match divergent::reason(name) {
        Some(why) => format!("{symbol}: no Darwin-compatible implementation on this host: {why}"),
        None if stdio_refuses(name) => format!(
            "{symbol}: not implemented by the builtin stdio, and the host's cannot use its streams"
        ),
        None if strict::enabled() && host_symbol(name).is_some() => format!(
            "{symbol}: nothing in this tree implements it, and {} withdraws the host's own",
            strict::VAR
        ),
        None => format!("{symbol}: nothing on this host defines it"),
    }
}

/// A source of answers registered by the crate above this one.
///
/// `darwin` cannot depend on `corefoundation` - the dependency runs the other
/// way - but the two have to agree: a name the loader binds to this tree's
/// `CFStringGetLength` must also be the one a `dlsym` finds, or a guest gets an
/// import that works and a `dlsym` of the same name that comes back null. The
/// runtime registers itself here at startup and this crate asks.
static REGISTERED: std::sync::RwLock<Vec<fn(&str) -> Option<u64>>> =
    std::sync::RwLock::new(Vec::new());

/// Guest symbol families owned by a compatibility provider must not fall back
/// to host libraries when that provider has no implementation.
static HOST_FILTERS: std::sync::RwLock<Vec<fn(&str) -> bool>> = std::sync::RwLock::new(Vec::new());

/// Register a filter for guest and shim fallback, after implemented symbols.
/// Explicit internal `host_symbol` lookups are unaffected.
pub fn register_host_symbol_filter(filter: fn(&str) -> bool) {
    HOST_FILTERS.write().unwrap().push(filter);
}

fn host_fallback_allowed(name: &str) -> bool {
    HOST_FILTERS
        .read()
        .unwrap()
        .iter()
        .all(|filter| filter(name))
}

/// Add a source of answers, consulted by [`symbol`] alongside this crate's own.
///
/// Idempotent only by the caller's discipline: registering twice costs a second
/// lookup and answers the same.
pub fn register_provider(source: fn(&str) -> Option<u64>) {
    REGISTERED
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push(source);
}

fn registered(name: &str) -> Option<u64> {
    REGISTERED
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find_map(|source| source(name))
}

/// Guest-aware overrides enabled on every host, even with native libc.
static DEFAULT_MODULES: &[exports::Module] = &[
    idle::MODULE,
    errno::MODULE,
    sysctl::MODULE,
    sysinfo::MODULE,
    commoncrypto::MODULE,
    math::MODULE,
    spawn::MODULE,
    icu::MODULE,
    progname::MODULE,
];

/// Builtin libc modules shared by macOS and non-macOS hosts.
static SHARED_MODULES: &[exports::Module] = &[
    pthread::MODULE,
    stdio::MODULE,
    clock::MODULE,
    glob::MODULE,
    regex::MODULE,
    resolv::MODULE,
    string::MODULE,
    strtofp::MODULE,
    stdlib::MODULE,
    stat::MODULE,
    fcntl::entry::MODULE,
    stackprobe::MODULE,
    atomics::MODULE,
    unfair_lock::MODULE,
    fortify::MODULE,
    env::MODULE,
    humanize::MODULE,
    sysdir::MODULE,
    signal::entry::MODULE,
    mach::MODULE,
    fs::MODULE,
    copyfile::MODULE,
    xattr::MODULE,
    passwd::MODULE,
    audit::MODULE,
    dirent::MODULE,
    libproc::MODULE,
    fts::MODULE,
    attrlist::MODULE,
    process::MODULE,
    credentials::MODULE,
    pam::MODULE,
    sip::MODULE,
    dispatch::MODULE,
    net::MODULE,
    kqueue::MODULE,
    modes::MODULE,
];

/// Builtin modules whose guest exports are not yet ported to macOS.
#[cfg(not(target_os = "macos"))]
static NONPORTABLE_MODULES: &[exports::Module] = &[
    compat::MODULE,
    bsd::MODULE,
    termios::MODULE,
    sched::MODULE,
    random::MODULE,
    ioctl::MODULE,
    ulock::MODULE,
    data::MODULE,
    xlocale::MODULE,
    malloc::MODULE,
    vmmap::MODULE,
    poll::MODULE,
    select::MODULE,
    uuid::MODULE,
    callback::MODULE,
    rlimit::MODULE,
    shm::MODULE,
    oslog::MODULE,
    backtrace::MODULE,
    #[cfg(target_arch = "x86_64")]
    printf_l::MODULE,
    acl::MODULE,
    fmtcheck::MODULE,
    langinfo::MODULE,
    #[cfg(target_os = "linux")]
    utmpx::MODULE,
    vis::MODULE,
    wait::MODULE,
    #[cfg(target_os = "linux")]
    getopt::MODULE,
];

#[cfg(target_os = "macos")]
static NONPORTABLE_MODULES: &[exports::Module] = &[];

/// The same selected modules drive import publication and direct symbol lookup.
/// Each name belongs to only one module, so publishing order does not matter.
fn modules() -> impl Iterator<Item = &'static exports::Module> {
    let builtin = if libc_backend::using_builtin() {
        SHARED_MODULES
    } else {
        &[]
    };
    [DEFAULT_MODULES, builtin, NONPORTABLE_MODULES]
        .into_iter()
        .flatten()
}

fn implemented(name: &str) -> Option<u64> {
    registered(name).or_else(|| modules().find_map(|module| (module.symbol)(name)))
}

pub fn is_implemented(symbol: &str) -> bool {
    implemented(base_name(symbol)).is_some()
}

pub const LIBRARY_NAMES: &[&str] = &[
    "/usr/lib/libSystem.B.dylib",
    "/usr/lib/libSystem.dylib",
    "/usr/lib/libc.dylib",
    "/usr/lib/libm.dylib",
    "/usr/lib/libdl.dylib",
    "/usr/lib/libpthread.dylib",
    "/usr/lib/libinfo.dylib",
    "/usr/lib/libpoll.dylib",
    "/usr/lib/libproc.dylib",
    "/usr/lib/libdbm.dylib",
    "/usr/lib/librpcsvc.dylib",
    "/usr/lib/libgcc_s.1.dylib",
    // OpenPAM, which `crate::pam` is on hosts without a Darwin one.
    "/usr/lib/libpam.2.dylib",
    "/usr/lib/libpam.dylib",
    "/usr/lib/system/libcompiler_rt.dylib",
    "/usr/lib/system/libmacho.dylib",
    "/usr/lib/system/libsystem_blocks.dylib",
    "/usr/lib/system/libsystem_c.dylib",
    "/usr/lib/system/libsystem_collections.dylib",
    "/usr/lib/system/libsystem_kernel.dylib",
    "/usr/lib/system/libsystem_malloc.dylib",
    "/usr/lib/system/libsystem_platform.dylib",
    "/usr/lib/system/libsystem_pthread.dylib",
    // An umbrella over libSystem, which is what a program that loads it as a
    // bundle to look up `free` or `malloc` by name expects to find there.
    "/System/Library/Frameworks/System.framework/Versions/B/System",
    "/System/Library/Frameworks/System.framework/System",
];

pub fn provider() -> machish::resolve::SyntheticDylib {
    // Before the guest's initializers can register an exit handler of their own.
    stdio::register_cleanup();
    // The zone registry, and its fork handlers ahead of any the guest installs.
    #[cfg(not(target_os = "macos"))]
    malloc::init();
    let mut lib = machish::resolve::SyntheticDylib::new("<darwin>");
    lib.flat(true);
    for name in LIBRARY_NAMES {
        lib.alias(name);
    }
    let out = &mut exports::NonNull(&mut lib);
    for module in modules() {
        (module.provide)(out);
    }
    lib
}

#[cfg(test)]
mod tests {
    use super::*;
    use machish::fixups::{LibraryRef, SymbolRef};
    use machish::resolve::SymbolProvider;

    fn published(lib: &machish::resolve::SyntheticDylib, name: &str) -> Option<u64> {
        lib.resolve(&SymbolRef {
            name,
            library: LibraryRef::Flat,
            weak_import: false,
        })
        .map(|resolved| resolved.addr)
    }

    #[test]
    fn libc_mode_child() {
        let Ok(want) = std::env::var("ROSEMARY_LIBC_TEST_CHILD") else {
            return;
        };
        assert_eq!(libc_backend::using_builtin(), want == "builtin");
        let builtin = want == "builtin";
        let lib = provider();
        for (group, enabled) in [
            (DEFAULT_MODULES, true),
            (SHARED_MODULES, builtin),
            (NONPORTABLE_MODULES, true),
        ] {
            for module in group {
                for name in (module.names)() {
                    let mach_o = format!("_{name}");
                    let expected = enabled.then(|| (module.symbol)(name).unwrap());
                    assert_eq!(is_implemented(&mach_o), enabled, "{mach_o}");
                    assert_eq!(published(&lib, &mach_o), expected, "{mach_o}");
                    if enabled {
                        assert_eq!(symbol(&mach_o), expected, "{mach_o}");
                    }
                }
            }
        }
        for name in ["_copyfile", "_kevent"] {
            assert_eq!(is_implemented(name), builtin, "{name}");
            assert_eq!(published(&lib, name).is_some(), builtin, "{name}");
        }
        assert!(is_implemented("_sysctl"));
        for name in [
            "_clock_gettime",
            "_glob",
            #[cfg(any(target_os = "macos", target_arch = "x86_64"))]
            "_res_9_init",
            "_strerror",
            "_pthread_mutex_lock",
            "_pthread_attr_init",
            "_pthread_once",
            "_pthread_create",
            "_getenv",
            "_setenv",
            "_unsetenv",
            "_putenv",
            "_getbsize",
        ] {
            assert_eq!(is_implemented(name), want == "builtin", "{name}");
        }
        for name in [
            "glob",
            "pthread_mutex_lock",
            "pthread_attr_init",
            "pthread_once",
            "pthread_create",
            "getenv",
            "setenv",
            "unsetenv",
            "putenv",
            "getbsize",
        ] {
            let addr = symbol(name).expect("native or builtin symbol");
            assert_eq!(
                addr,
                if want == "builtin" {
                    (glob::MODULE.symbol)(name)
                        .or_else(|| (pthread::MODULE.symbol)(name))
                        .or_else(|| (env::MODULE.symbol)(name))
                        .or_else(|| (humanize::MODULE.symbol)(name))
                        .unwrap()
                } else {
                    host_symbol(name).unwrap()
                }
            );
        }
    }

    #[test]
    fn libc_mode_selects_the_same_symbols_for_imports_and_dlsym() {
        let native = if cfg!(target_os = "macos") {
            "native"
        } else {
            "builtin"
        };
        for (setting, want, success) in [
            (None, native, true),
            (Some("native"), native, true),
            (Some("builtin"), "builtin", true),
            (Some("invalid"), native, !cfg!(target_os = "macos")),
        ] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "tests::libc_mode_child"]);
            command.env("ROSEMARY_LIBC_TEST_CHILD", want);
            match setting {
                Some(value) => {
                    command.env(libc_backend::VAR, value);
                }
                None => {
                    command.env_remove(libc_backend::VAR);
                }
            }
            let output = command.output().unwrap();
            assert_eq!(
                output.status.success(),
                success,
                "{setting:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn no_module_defines_a_name_another_one_does() {
        let mut seen: Vec<(&str, &str)> = Vec::new();
        for module in DEFAULT_MODULES
            .iter()
            .chain(SHARED_MODULES)
            .chain(NONPORTABLE_MODULES)
        {
            for name in (module.names)() {
                if let Some((_, first)) = seen.iter().find(|(n, _)| *n == name) {
                    panic!("{name} is defined by both {first} and {}", module.path);
                }
                seen.push((name, module.path));
            }
        }
    }

    #[test]
    fn a_published_name_is_the_one_dlsym_answers() {
        let lib = provider();
        for module in modules() {
            for name in (module.names)() {
                let mach_o = format!("_{name}");
                let expected = (module.symbol)(name).expect("listed symbol");
                assert_eq!(published(&lib, &mach_o), Some(expected), "{mach_o}");
                assert_eq!(symbol(&mach_o), Some(expected), "{mach_o}");
            }
        }
    }

    #[test]
    fn nothing_is_both_claimed_safe_and_divergent() {
        let overlap: Vec<&str> = passthrough::CLAIMED
            .iter()
            .copied()
            .filter(|n| divergent::contains(n))
            .collect();
        assert!(
            overlap.is_empty(),
            "claimed safe and also denied: {overlap:?}"
        );
    }

    #[test]
    fn every_path_taking_shim_is_reachable() {
        if !libc_backend::using_builtin() {
            return;
        }
        let unreachable: Vec<&str> = [
            "getenv", "setenv", "putenv", "unsetenv", "chdir", "getcwd", "unlink", "rmdir",
            "remove", "mkdir", "chmod", "chown", "lchown", "truncate", "link", "rename", "symlink",
            "freopen", "mkstemp", "mkdtemp",
        ]
        .into_iter()
        .filter(|name| implemented(name).is_none())
        .collect();
        assert!(
            unreachable.is_empty(),
            "written but not reachable - is the module in a module group? {unreachable:?}"
        );
    }

    #[test]
    fn nothing_implemented_is_also_claimed_safe() {
        let overlap: Vec<&str> = passthrough::CLAIMED
            .iter()
            .copied()
            .filter(|n| implemented(n).is_some())
            .collect();
        assert!(
            overlap.is_empty(),
            "claimed safe and also implemented here: {overlap:?}"
        );
    }

    #[test]
    fn the_mach_o_underscore_is_not_part_of_the_name() {
        assert_eq!(c_name("_sysctl"), "sysctl");
        assert_eq!(c_name("__error"), "_error");
        assert_eq!(c_name("sysctl"), "sysctl");
    }

    /// A `$` suffix names the same function
    #[test]
    fn a_variant_suffix_is_not_part_of_the_name() {
        assert_eq!(base_name("_close$NOCANCEL"), "close");
        assert_eq!(base_name("_fopen$DARWIN_EXTSN"), "fopen");
        assert_eq!(base_name("_getdirentries$INODE64"), "getdirentries");
        #[cfg(not(target_os = "macos"))]
        assert!(divergent::contains(base_name("_getdirentries$INODE64")));
        assert_eq!(base_name("_$NOCANCEL"), "$NOCANCEL");
        assert_eq!(base_name("_close$"), "close$");
    }
}
