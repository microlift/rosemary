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

#[cfg(not(target_os = "macos"))]
pub mod acl;
mod atomics;
pub mod attrlist;
#[cfg(not(target_os = "macos"))]
pub mod audit;
pub mod backtrace;
pub mod bsd;
pub mod callback;
#[cfg(not(target_os = "macos"))]
pub mod cg_unavailable;
#[cfg(not(target_os = "macos"))]
pub mod as_unavailable;
#[cfg(not(target_os = "macos"))]
pub mod gfx_unavailable;
pub mod clock;
#[cfg(not(target_os = "macos"))]
pub mod media_unavailable;
pub mod commoncrypto;
#[cfg(not(target_os = "macos"))]
pub mod compat;
pub mod copyfile;
pub mod data;
pub mod dirent;
pub mod dispatch;
pub mod divergent;
#[cfg(not(target_os = "macos"))]
pub mod env;
pub mod errno;
pub mod fcntl;
pub mod fmtcheck;
#[cfg(not(target_os = "macos"))]
pub mod fortify;
pub mod fs;
#[cfg(not(target_os = "macos"))]
pub mod fts;
#[cfg(not(target_os = "macos"))]
pub mod glob;
pub mod heap;
#[cfg(not(target_os = "macos"))]
pub mod humanize;
pub mod idle;
pub mod ioctl;
pub mod kqueue;
#[cfg(not(target_os = "macos"))]
pub mod data_unavailable;
pub mod langinfo;
#[cfg(not(target_os = "macos"))]
pub mod last_unavailable;
pub mod libproc;
pub mod locale;
pub mod mach;
pub mod malloc;
pub mod math;
#[cfg(not(target_os = "macos"))]
pub mod modes;
pub mod net;
mod oslog;
pub mod passthrough;
pub mod passwd;
pub mod poll;
pub mod printf_l;
#[cfg(not(target_os = "macos"))]
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
pub mod spawn;
pub mod stackprobe;
pub mod stat;
pub mod stdio;
#[cfg(target_os = "linux")]
pub mod stream;
pub mod strict;
pub mod string;
pub mod sysctl;
pub mod sysinfo;
#[cfg(not(target_os = "macos"))]
pub mod termios;
pub mod tls;
pub mod translation;
pub mod ulock;
pub mod unfair_lock;
pub mod unavailable;
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
pub mod xpc;

/// The environment a guest's `main` receives as `envp`, as `NAME=VALUE`.
///
/// Off Darwin that is [`env`]'s block, corrected for the prefix; on Darwin the
/// guest shares this process's environment.
pub fn guest_environment() -> Vec<CString> {
    #[cfg(not(target_os = "macos"))]
    {
        env::entries()
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        // Bytes, not text: a value is an arbitrary byte string.
        std::env::vars_os()
            .filter_map(|(k, v)| CString::new([k.as_bytes(), b"=", v.as_bytes()].concat()).ok())
            .collect()
    }
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
/// GLU is a separate library off Darwin - `gluOrtho2D` is in the OpenGL
/// framework there and in `libGLU.so.1` here - so it is a second `dlopen` and
/// not an oversight.
///
/// GL itself is deliberately **not** opened here. Its entry points have to come
/// out of the library SDL made the context against, which is not necessarily
/// this loader's `libGL.so.1`, and a global one would answer every `gl` name
/// first. See `rosetta3::opengl`.
fn load_host_libraries() {
    #[cfg(not(target_os = "macos"))]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            for soname in [c"libm.so.6", c"libGLU.so.1"] {
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
    if divergent::contains(name) || strict::enabled() {
        return None;
    }
    host_symbol(name)
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
    if divergent::contains(name) {
        return None;
    }
    host_symbol(name)
}

/// Why [`symbol`] declined `symbol`, as a sentence.
pub fn refusal(symbol: &str) -> String {
    let name = base_name(symbol);
    match divergent::reason(name) {
        Some(why) => format!("{symbol}: no Darwin-compatible implementation on this host: {why}"),
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

/// Every module this crate publishes, once.
///
/// `provider` and `implemented` are both this list. They used to be two, in two
/// hand-kept orders, and a module in one and not the other is a name the loader
/// binds and a `dlsym` answers null for - which is what had happened to
/// `atomics` and `oslog`.
///
/// The order is the publishing order. Nothing here may define a name another
/// entry defines - see `no_module_defines_a_name_another_one_does`, which is
/// what makes the order not matter.
static MODULES: &[exports::Module] = &[
    idle::MODULE,
    stdio::MODULE,
    errno::MODULE,
    sysctl::MODULE,
    sysinfo::MODULE,
    mach::MODULE,
    clock::MODULE,
    commoncrypto::MODULE,
    math::MODULE,
    #[cfg(not(target_os = "macos"))]
    pthread::MODULE,
    #[cfg(not(target_os = "macos"))]
    compat::MODULE,
    #[cfg(not(target_os = "macos"))]
    fortify::MODULE,
    #[cfg(not(target_os = "macos"))]
    env::MODULE,
    #[cfg(not(target_os = "macos"))]
    fs::MODULE,
    #[cfg(not(target_os = "macos"))]
    glob::MODULE,
    #[cfg(not(target_os = "macos"))]
    regex::MODULE,
    #[cfg(not(target_os = "macos"))]
    termios::MODULE,
    #[cfg(not(target_os = "macos"))]
    sched::MODULE,
    #[cfg(not(target_os = "macos"))]
    random::MODULE,
    #[cfg(not(target_os = "macos"))]
    ioctl::MODULE,
    ulock::MODULE,
    unfair_lock::MODULE,
    spawn::MODULE,
    dispatch::MODULE,
    data::MODULE,
    #[cfg(not(target_os = "macos"))]
    atomics::MODULE,
    #[cfg(not(target_os = "macos"))]
    xlocale::MODULE,
    #[cfg(not(target_os = "macos"))]
    stat::MODULE,
    #[cfg(not(target_os = "macos"))]
    malloc::MODULE,
    #[cfg(not(target_os = "macos"))]
    vmmap::MODULE,
    #[cfg(not(target_os = "macos"))]
    string::MODULE,
    #[cfg(not(target_os = "macos"))]
    poll::MODULE,
    #[cfg(not(target_os = "macos"))]
    select::MODULE,
    #[cfg(not(target_os = "macos"))]
    net::MODULE,
    #[cfg(not(target_os = "macos"))]
    xattr::MODULE,
    #[cfg(not(target_os = "macos"))]
    copyfile::MODULE,
    #[cfg(not(target_os = "macos"))]
    uuid::MODULE,
    #[cfg(not(target_os = "macos"))]
    signal::entry::MODULE,
    #[cfg(not(target_os = "macos"))]
    fcntl::entry::MODULE,
    #[cfg(not(target_os = "macos"))]
    bsd::MODULE,
    #[cfg(not(target_os = "macos"))]
    callback::MODULE,
    #[cfg(not(target_os = "macos"))]
    passwd::MODULE,
    #[cfg(not(target_os = "macos"))]
    audit::MODULE,
    #[cfg(not(target_os = "macos"))]
    dirent::MODULE,
    #[cfg(not(target_os = "macos"))]
    rlimit::MODULE,
    #[cfg(not(target_os = "macos"))]
    shm::MODULE,
    #[cfg(not(target_os = "macos"))]
    libproc::MODULE,
    #[cfg(not(target_os = "macos"))]
    oslog::MODULE,
    #[cfg(not(target_os = "macos"))]
    kqueue::MODULE,
    #[cfg(not(target_os = "macos"))]
    backtrace::MODULE,
    #[cfg(not(target_os = "macos"))]
    stackprobe::MODULE,
    #[cfg(all(not(target_os = "macos"), target_arch = "x86_64"))]
    printf_l::MODULE,
    #[cfg(all(not(target_os = "macos"), target_arch = "x86_64"))]
    resolv::MODULE,
    #[cfg(not(target_os = "macos"))]
    acl::MODULE,
    #[cfg(not(target_os = "macos"))]
    fmtcheck::MODULE,
    #[cfg(not(target_os = "macos"))]
    fts::MODULE,
    #[cfg(not(target_os = "macos"))]
    humanize::MODULE,
    #[cfg(not(target_os = "macos"))]
    attrlist::MODULE,
    #[cfg(not(target_os = "macos"))]
    langinfo::MODULE,
    #[cfg(not(target_os = "macos"))]
    modes::MODULE,
    #[cfg(not(target_os = "macos"))]
    progname::MODULE,
    #[cfg(target_os = "linux")]
    stream::MODULE,
    #[cfg(not(target_os = "macos"))]
    utmpx::MODULE,
    #[cfg(not(target_os = "macos"))]
    vis::MODULE,
    #[cfg(not(target_os = "macos"))]
    wait::MODULE,
    unavailable::MODULE,
    #[cfg(not(target_os = "macos"))]
    cg_unavailable::MODULE,
    #[cfg(not(target_os = "macos"))]
    as_unavailable::MODULE,
    #[cfg(not(target_os = "macos"))]
    gfx_unavailable::MODULE,
    #[cfg(not(target_os = "macos"))]
    media_unavailable::MODULE,
    #[cfg(not(target_os = "macos"))]
    last_unavailable::MODULE,
    #[cfg(not(target_os = "macos"))]
    data_unavailable::MODULE,
    xpc::MODULE,
];

fn implemented(name: &str) -> Option<u64> {
    registered(name).or_else(|| exports::Module::resolve(MODULES, name))
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
    "/usr/lib/system/libcompiler_rt.dylib",
    "/usr/lib/system/libmacho.dylib",
    "/usr/lib/system/libsystem_blocks.dylib",
    "/usr/lib/system/libsystem_c.dylib",
    "/usr/lib/system/libsystem_collections.dylib",
    "/usr/lib/system/libsystem_kernel.dylib",
    "/usr/lib/system/libsystem_malloc.dylib",
    "/usr/lib/system/libsystem_platform.dylib",
    "/usr/lib/system/libsystem_pthread.dylib",
];

pub fn provider() -> machish::resolve::SyntheticDylib {
    let mut lib = machish::resolve::SyntheticDylib::new("<darwin>");
    lib.flat(true);
    for name in LIBRARY_NAMES {
        lib.alias(name);
    }
    let out = &mut exports::NonNull(&mut lib);
    exports::Module::provide_all(MODULES, out);
    lib
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Every name more than one module claims, and the modules that claim it.
    ///
    /// **This used to `assert!` inside the loop, which made it assert nothing.**
    /// `sched_yield` is claimed by both `darwin::idle` and `darwin::pthread`,
    /// and `idle` is early in `MODULES` - so the test failed on that one
    /// collision and never looked at the other ~40 modules. A check that stops at
    /// the first failure is a check on one fact, and a second collision anywhere
    /// else in the table was invisible until this one was fixed.
    ///
    /// **Returned rather than asserted, so a caller can ask the question without
    /// the answer deciding for it** - which is what lets
    /// `the_census_reports_every_duplicate_and_not_the_first` below test the
    /// *collection* against a table built to have several duplicates, instead of
    /// against the real table where the first one and the only one are the same
    /// name and a check that stopped early would look identical.
    ///
    /// Owners are sorted so the answer is a fact about *which* modules claim a
    /// name rather than about the order the table happens to be in; the names are
    /// sorted so the message is stable and the next person to add a duplicate
    /// sees their own in the list rather than a stale first entry.
    fn collisions(modules: &[exports::Module]) -> Vec<(String, Vec<&'static str>)> {
        let mut owners: HashMap<&str, Vec<&str>> = HashMap::new();
        for module in modules {
            for name in (module.names)() {
                owners.entry(name).or_default().push(module.path);
            }
        }
        let mut found: Vec<(String, Vec<&'static str>)> = owners
            .into_iter()
            .filter(|(_, paths)| paths.len() > 1)
            .map(|(name, mut paths)| {
                paths.sort_unstable();
                (name.to_string(), paths)
            })
            .collect();
        found.sort();
        found
    }

    #[test]
    fn no_module_defines_a_name_another_one_does() {
        let collisions = collisions(MODULES);
        assert!(
            collisions.is_empty(),
            "{} name(s) defined by more than one module: {}",
            collisions.len(),
            collisions
                .iter()
                .map(|(name, paths)| format!("{name} by {}", paths.join(" and ")))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    /// The control for the test above, and the only thing that can tell it apart
    /// from a vacuous one.
    ///
    /// **On the real table the two implementations are indistinguishable.** The
    /// real table's first duplicate and its only duplicate are both `sched_yield`,
    /// so a census that stops at the first and a census that collects every one
    /// produce the same answer on this tree today. The census above therefore
    /// cannot, by itself, tell a real check from the thing it replaced - which is
    /// precisely the failure mode `sched_yield` at index 0 was hiding, and why
    /// this file needed the second test rather than only the first.
    ///
    /// So this asks the same question against a table built to have **three**
    /// independent duplicates, none of them at index 0, and asserts an answer
    /// written out literally. A census that stopped at the first would have no
    /// value to return at all, so it cannot pass this; one that returned only the
    /// alphabetically-first would fail it too, which is the second way to be
    /// incomplete.
    #[test]
    fn the_census_reports_every_duplicate_and_not_the_first() {
        // Named `fn`s rather than closures: `Module::names` is a `fn()` pointer
        // and a closure cannot capture the list it was handed.
        fn first() -> exports::Names {
            vec!["shared", "solo"]
        }
        fn second() -> exports::Names {
            vec!["shared"]
        }
        fn third() -> exports::Names {
            vec!["twin", "also_shared"]
        }
        fn fourth() -> exports::Names {
            vec!["twin", "also_shared"]
        }
        fn module(path: &'static str, names: fn() -> exports::Names) -> exports::Module {
            exports::Module {
                path,
                names,
                symbol: |_| None,
                provide: |_: &mut dyn exports::Exports| {},
            }
        }
        // `shared` is the first collision in *table* order and `also_shared` is
        // the first in *name* order, so the two cannot be confused for each other
        // either. `twin` is a third, and neither first.
        let table = [
            module("test::first", first),
            module("test::second", second),
            module("test::third", third),
            module("test::fourth", fourth),
        ];
        assert_eq!(
            collisions(&table),
            vec![
                (
                    "also_shared".to_string(),
                    vec!["test::fourth", "test::third"]
                ),
                ("shared".to_string(), vec!["test::first", "test::second"]),
                ("twin".to_string(), vec!["test::fourth", "test::third"]),
            ],
            "the census must name every duplicate, in name order, with every owner"
        );
    }

    /// What the census says about the real table, pinned in a test that passes.
    ///
    /// The test above is **red on purpose** until the `sched_yield` duplicate is
    /// decided one way or the other, and a red test is a bad place to keep a
    /// census: a second duplicate arriving changes that failure's *message*, and
    /// a reader who has already learned to expect `sched_yield` reads straight
    /// past the rest of the line. So the count is asserted where it can fail by
    /// itself - a **new** duplicate is a separate failure here, in a suite that
    /// was otherwise green, and the one duplicate that is known is named by a
    /// test that passes.
    ///
    /// This does not soften the red one and it does not remove the duplicate: it
    /// is the same fact, in a place where adding a collision cannot hide behind
    /// a failure that is already expected.
    #[test]
    fn exactly_one_duplicate_is_known_and_it_is_sched_yield() {
        assert_eq!(
            collisions(MODULES),
            vec![(
                "sched_yield".to_string(),
                vec!["darwin::idle", "darwin::pthread"],
            )],
            "the duplicate set changed: a name is now defined by more than one \
             module that was not before, or `sched_yield` has been decided. The \
             full set is above, with every module that claims each name"
        );
    }

    #[test]
    fn a_published_name_is_the_one_dlsym_answers() {
        for module in MODULES {
            for name in (module.names)() {
                let mach_o = format!("_{name}");
                assert!(
                    symbol(&mach_o).is_some(),
                    "{}: {mach_o} is published and unanswered",
                    module.path
                );
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

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn every_path_taking_shim_is_reachable() {
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
            "written but not reachable - is the module in MODULES? {unreachable:?}"
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
        assert!(divergent::contains(base_name("_getdirentries$INODE64")));
        assert_eq!(base_name("_$NOCANCEL"), "$NOCANCEL");
        assert_eq!(base_name("_close$"), "close$");
    }
}
