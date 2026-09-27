use exports::Exports;
use machish::resolve::{NativeDylibProvider, SyntheticDylib};

use crate::objc::host::{Backend, backend};

pub fn using_builtin() -> bool {
    backend() == Backend::Builtin
}

/// Tell `darwin` where to find these, so that a `dlsym` and the loader agree.
///
/// Called from everything below rather than from start-up: `darwin::symbol` is
/// asked during linking, which happens before `startup::run`, and a registration
/// that lands after the first question is a registration that did not happen.
fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        darwin::register_provider(symbol);
        register_dlopen_names();
        install_block_runtime();
        push_main_pool();
    });
}

/// Tell `darwin`'s libdispatch which block runtime this process is using.
///
/// `dispatch_async` has to copy the block it is given, because the block outlives its
/// caller's frame - but a block carries its runtime's `_NSConcreteMallocBlock` in its
/// `isa`, so whichever runtime copies it has to be the one that later releases it.
/// `darwin` therefore implements neither and asks here, where the selection that
/// [`crate::objc::host`] already made is known.
fn install_block_runtime() {
    let (Some(copy), Some(release)) = (
        crate::objc::host::symbol("_Block_copy"),
        crate::objc::host::symbol("_Block_release"),
    ) else {
        return;
    };
    darwin::dispatch::block::set_runtime(copy, release);
}

struct Framework {
    leaf: &'static str,
    version: char,
    umbrellas: &'static [&'static str],
    provide: Option<fn(&mut dyn exports::Exports)>,
    data: Option<fn() -> Vec<(String, u64)>>,
    symbol: Option<fn(&str) -> Option<u64>>,
}

static FRAMEWORKS: &[Framework] = &[
    Framework {
        leaf: "CoreFoundation",
        version: 'A',
        umbrellas: &[],
        provide: Some(corefoundation::provide),
        data: Some(corefoundation::data_symbols),
        symbol: Some(corefoundation::symbol),
    },
    Framework {
        leaf: "Foundation",
        version: 'C',
        umbrellas: &[],
        provide: Some(foundation::provide),
        data: Some(foundation::data_symbols),
        symbol: Some(foundation::symbol),
    },
    Framework {
        leaf: "IOKit",
        version: 'A',
        umbrellas: &[],
        provide: Some(iokit::provide),
        data: Some(iokit::data_symbols),
        symbol: Some(iokit::symbol),
    },
    Framework {
        leaf: "ForceFeedback",
        version: 'A',
        umbrellas: &[],
        provide: Some(forcefeedback::provide),
        data: None,
        symbol: Some(forcefeedback::symbol),
    },
    Framework {
        leaf: "AppKit",
        version: 'C',
        umbrellas: &["Cocoa"],
        provide: Some(appkit::provide),
        data: Some(appkit::data_symbols),
        symbol: Some(appkit::symbol),
    },
    Framework {
        leaf: "ApplicationServices",
        version: 'A',
        umbrellas: &[],
        provide: Some(applicationservices::provide),
        data: None,
        symbol: Some(applicationservices::symbol),
    },
    Framework {
        leaf: "CoreServices",
        version: 'A',
        umbrellas: &["Carbon"],
        provide: Some(coreservices::provide),
        data: None,
        symbol: Some(coreservices::symbol),
    },
    Framework {
        leaf: "AudioToolbox",
        version: 'A',
        umbrellas: &[],
        provide: Some(audiotoolbox::provide),
        data: None,
        symbol: Some(audiotoolbox::symbol),
    },
    Framework {
        leaf: "CoreVideo",
        version: 'A',
        umbrellas: &[],
        provide: Some(corevideo::provide),
        data: None,
        symbol: Some(corevideo::symbol),
    },
    Framework {
        leaf: "CoreAudio",
        version: 'A',
        umbrellas: &[],
        provide: Some(coreaudio::provide),
        data: None,
        symbol: Some(coreaudio::symbol),
    },
    Framework {
        leaf: "Contacts",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(contacts::data_symbols),
        symbol: None,
    },
    Framework {
        leaf: "AVFoundation",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(avfoundation::data_symbols),
        symbol: None,
    },
    // `provide: None` because the census says **zero** OpenDirectory *function*
    // imports - the guest reaches OpenDirectory only through `objc_msgSend`, so
    // classes and their selectors are the whole of the surface here.
    Framework {
        leaf: "OpenDirectory",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(opendirectory::data_symbols),
        symbol: Some(opendirectory::symbol),
    },
    // `provide: None` because the census says **zero** CoreLocation *function*
    // imports: the guest links the framework (LC_LOAD_DYLIB at 0x20fb) and
    // imports one class and two `double` constants, and the two constants are
    // already `darwin`'s named zeroed refusals. So the class and its selectors
    // are the whole of the surface here.
    Framework {
        leaf: "CoreLocation",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(corelocation::data_symbols),
        symbol: Some(corelocation::symbol),
    },
    // `provide: None` for the reason the OpenDirectory row above gives: zero
    // CoreWLAN *function* imports - one data import, one class. That is
    // provable rather than sampled, which matters here: this host has no real
    // CoreWLAN, so every CoreWLAN import would be unbound and would appear in
    // the census, and the log carries exactly one line for it.
    Framework {
        leaf: "CoreWLAN",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(corewlan::data_symbols),
        symbol: Some(corewlan::symbol),
    },
    Framework {
        leaf: "Security",
        version: 'A',
        umbrellas: &[],
        provide: Some(security::provide),
        data: Some(security::data_symbols),
        symbol: Some(security::symbol),
    },
    Framework {
        leaf: "GameController",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(gamecontroller::data_symbols),
        symbol: Some(gamecontroller::symbol),
    },
    Framework {
        leaf: "WebKit",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(webkit::data_symbols),
        symbol: Some(webkit::symbol),
    },
    Framework {
        leaf: "Metal",
        version: 'A',
        umbrellas: &[],
        provide: Some(metal::provide),
        data: Some(metal::data_symbols),
        symbol: Some(metal::symbol),
    },
];

fn paths_of(framework: &Framework) -> Vec<String> {
    let mut out = Vec::new();
    for leaf in std::iter::once(framework.leaf).chain(framework.umbrellas.iter().copied()) {
        let version = if leaf == framework.leaf {
            framework.version
        } else {
            'A'
        };
        out.push(format!(
            "/System/Library/Frameworks/{leaf}.framework/Versions/{version}/{leaf}"
        ));
        out.push(format!(
            "/System/Library/Frameworks/{leaf}.framework/{leaf}"
        ));
        out.push(leaf.to_string());
    }
    out
}

fn register_dlopen_names() {
    if !using_builtin() {
        return;
    }
    for framework in FRAMEWORKS {
        let paths = paths_of(framework);
        let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();
        crate::dyld::register_synthetic(&borrowed, symbol);
    }
    register_opengl_framework();
    register_openal_framework();
    register_system_libraries();
}

fn register_system_libraries() {
    crate::dyld::register_synthetic(darwin::LIBRARY_NAMES, crate::dyld::system_symbol);
    crate::dyld::register_synthetic(
        &["/usr/lib/libobjc.A.dylib", "/usr/lib/libobjc.dylib"],
        objc_symbol,
    );
    // `/usr/lib/libsandbox.1.dylib` is the install name every helper image
    // declares (`LC_LOAD_DYLIB`, first entry, ahead of `libSystem.B.dylib`),
    // and it is not in `darwin::LIBRARY_NAMES`. A bind does not need this - the
    // loader's chain is global, which is how `_sandbox_free_error` resolves from
    // this library today - but a `dlopen` of the path does, and the leaf spelling
    // is registered too because `synthetic::find` matches on either.
    crate::dyld::register_synthetic(
        &["/usr/lib/libsandbox.1.dylib", "libsandbox.1.dylib"],
        symbol,
    );
}

fn objc_symbol(name: &str) -> Option<u64> {
    symbol(name).or_else(|| crate::objc::host::symbol(name))
}

fn register_opengl_framework() {
    if !crate::opengl::interposed() {
        return;
    }
    crate::dyld::register_synthetic(crate::opengl::INSTALL_PATHS, crate::opengl::symbol);
}

fn register_openal_framework() {
    if !crate::openal::interposed() {
        return;
    }
    crate::dyld::register_synthetic(crate::openal::INSTALL_PATHS, crate::openal::symbol);
}

fn push_main_pool() {
    if !using_builtin() {
        return;
    }
    objc::arc::pool_push();
}

fn withheld(name: &str) -> bool {
    #[static_init::dynamic]
    static NAMES: Vec<String> = std::env::var("ROSETTA3_WITHHOLD")
        .unwrap_or_default()
        .split(',')
        .map(|n| n.trim().trim_start_matches('_').to_string())
        .filter(|n| !n.is_empty())
        .collect();

    let bare = name.trim_start_matches('_');
    let out = NAMES.iter().any(|n| n == bare);
    if out {
        eprintln!("rosetta3: {name}: withheld (ROSETTA3_WITHHOLD names it)");
    }
    out
}

pub fn provider() -> SyntheticDylib {
    install();
    let mut lib = SyntheticDylib::new("<rosetta3 frameworks>");
    lib.flat(true);
    if !using_builtin() {
        return lib;
    }
    // Before anything is published: `kCFNull` is the `NSNull` singleton, and
    // `NSNull` is Foundation's class.
    foundation::bootstrap();
    let out = &mut exports::Denying {
        inner: &mut lib,
        deny: withheld,
    };
    for framework in FRAMEWORKS {
        if let Some(provide) = framework.provide {
            provide(out);
        }
        if let Some(data) = framework.data {
            for (name, addr) in data() {
                out.data(&name, addr);
            }
        }
    }
    for (name, addr) in media_constants() {
        out.data(&name, addr);
    }
    // Not a framework, so it has no row in `FRAMEWORKS` and no
    // `/System/Library/Frameworks/...` path to be registered under: the five
    // helper images declare `LC_LOAD_DYLIB /usr/lib/libsandbox.1.dylib`, and the
    // two names it carries (`_sandbox_apply`, `_sandbox_init_with_parameters`)
    // were unbound - a zero in the slot and a jump through it, in five
    // processes. See `crates/sandbox/src/lib.rs` for the census that says which
    // of the fourteen `_sandbox_*` names the guest tolerates a refusal from and
    // which it traps on, and `register_system_libraries` for the install name.
    sandbox::provide(out);
    for (name, addr) in shimmed() {
        out.function(&format!("_{name}"), addr as usize as u64);
    }
    lib
}

const MEDIA_CONSTANTS: &[(&str, &str)] = &[
    (
        "_AVCaptureDeviceWasConnectedNotification",
        "AVCaptureDeviceWasConnectedNotification",
    ),
    (
        "_AVCaptureDeviceWasDisconnectedNotification",
        "AVCaptureDeviceWasDisconnectedNotification",
    ),
    (
        "_AVCaptureSessionDidStartRunningNotification",
        "AVCaptureSessionDidStartRunningNotification",
    ),
    (
        "_AVCaptureSessionDidStopRunningNotification",
        "AVCaptureSessionDidStopRunningNotification",
    ),
    (
        "_AVCaptureSessionRuntimeErrorNotification",
        "AVCaptureSessionRuntimeErrorNotification",
    ),
    ("_AVCaptureSessionErrorKey", "AVCaptureSessionErrorKey"),
    ("_AVMediaTypeVideo", "vide"),
    ("_AVMediaTypeAudio", "soun"),
    ("_AVMediaTypeText", "text"),
];

fn media_constants() -> Vec<(String, u64)> {
    corefoundation::base::string_globals(MEDIA_CONSTANTS)
}

fn shimmed() -> Vec<(&'static str, *const ())> {
    vec![
        ("NSLog", crate::shims::nslog::native::nslog() as *const ()),
        ("NSLogv", crate::shims::nslog::native::nslogv() as *const ()),
        (
            "NSRunAlertPanel",
            crate::shims::nslog::native::run_alert_panel() as *const (),
        ),
    ]
}

pub fn symbol(name: &str) -> Option<u64> {
    if !using_builtin() {
        return None;
    }
    foundation::bootstrap();
    // A dlsym answer must be the same address the flat provider binds, which is
    // why both come from `FRAMEWORKS`.
    FRAMEWORKS
        .iter()
        .filter_map(|f| f.symbol)
        .find_map(|symbol| symbol(name))
        // libclosure, which a shim forwards to by name - see
        // `crate::shims::block`. Nothing on a host off Darwin defines it, and it
        // has no framework of its own to be a row.
        .or_else(|| objc::block::symbol(name))
        // The host's GL, reached by `dlsym` rather than enumerated - see
        // `crate::opengl`, which publishes it through its own lazy provider.
        .or_else(|| crate::opengl::symbol(name))
        .or_else(|| crate::openal::symbol(name))
        // `libsandbox.1.dylib` is not a framework, so it has no row above; its
        // two names are registered for the loader in `provider` and have to be
        // registered for `dlsym` here, or the two would disagree about what
        // `_sandbox_apply` is - which is the bug the thread-local bootstrap in
        // PLAN.md §4 was.
        .or_else(|| sandbox::symbol(name))
}

pub fn set_main_bundle(executable: &std::path::Path) {
    install();
    if !using_builtin() {
        return;
    }
    let found = foundation::bundle::set_main(executable);
    appkit::metadata::publish(executable);
    if !found && std::env::var_os("ROSETTA3_TRACE_VM").is_some() {
        eprintln!(
            "rosetta3: bundle: {} is not inside a .app",
            executable.display()
        );
    }
}

pub fn deny_host_frameworks(provider: &mut NativeDylibProvider) {
    install();
    if !using_builtin() {
        return;
    }
    for prefix in [
        "_CF", "_NS", "_kCF", "_kNS", "_SC", "_kSC", "_CN", "_kCN", "_GC", "_kGC", "_Web",
    ] {
        provider.deny_prefix(prefix);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published() -> exports::Recording {
        let mut out = exports::Recording::default();
        foundation::bootstrap();
        for framework in FRAMEWORKS {
            if let Some(provide) = framework.provide {
                provide(&mut out);
            }
            if let Some(data) = framework.data {
                for (name, addr) in data() {
                    out.data(&name, addr);
                }
            }
        }
        out
    }

    #[test]
    fn no_framework_defines_a_symbol_another_one_does() {
        if !using_builtin() {
            return;
        }
        let out = published();
        let mut seen: Vec<&str> = Vec::new();
        let mut twice: Vec<&str> = Vec::new();
        for name in out.symbols() {
            if seen.contains(&name) {
                twice.push(name);
            } else {
                seen.push(name);
            }
        }
        assert!(twice.is_empty(), "defined more than once: {twice:?}");
    }

    #[test]
    fn every_framework_answers_a_dlopen_by_its_leaf() {
        for framework in FRAMEWORKS {
            let paths = paths_of(framework);
            for leaf in std::iter::once(framework.leaf).chain(framework.umbrellas.iter().copied()) {
                assert!(paths.iter().any(|p| p == leaf), "{leaf}");
                assert!(
                    paths.iter().any(
                        |p| p == &format!("/System/Library/Frameworks/{leaf}.framework/{leaf}")
                    ),
                    "{leaf}: the install name"
                );
            }
        }
    }

    #[test]
    fn a_published_function_is_the_one_dlsym_answers() {
        if !using_builtin() {
            return;
        }
        for framework in FRAMEWORKS {
            let (Some(provide), Some(answer)) = (framework.provide, framework.symbol) else {
                continue;
            };
            let mut out = exports::Recording::default();
            provide(&mut out);
            for (mach_o, address) in &out.functions {
                let bare = mach_o.strip_prefix('_').expect("the Mach-O underscore");
                assert_eq!(answer(bare), Some(*address), "{}: {mach_o}", framework.leaf);
            }
        }
    }

    /// The whole of the split-brain, as one claim: for **every** function any
    /// framework publishes, [`symbol`] - the `dlsym` path - answers at the
    /// address the flat provider publishes, the bind path.
    ///
    /// The two tests above do not have teeth together.
    /// [`Self::no_framework_defines_a_symbol_another_one_does`] only counts
    /// *names*, so two publishers that agree on the name and disagree on the
    /// address - or the same function reached through two different rows, which
    /// is a different bug with the same shape - pass it. And
    /// [`Self::a_published_function_is_the_one_dlsym_answers`] asks each
    /// framework about its *own* names, so a name published twice, by two
    /// frameworks, is never asked about from both sides at once. The
    /// duplicates this project has had - `_copyfile` and
    /// `_CGCursorIsVisible` - are both invisible to it, and the second was
    /// sitting in a passing suite.
    ///
    /// Functions only. A data slot is bound by address when the image loads and
    /// no code path here `dlsym`s one, and `corefoundation::symbol` answers
    /// almost no data name, so including them would assert a property nothing
    /// depends on. And this is the *default* configuration: `provider()` wraps
    /// the same publications in `exports::Denying`, and `ROSETTA3_WITHHOLD`
    /// deliberately makes the bind path publish strictly less than the `dlsym`
    /// path - an asymmetry that is the point of the knob and would otherwise be
    /// reported as a disagreement here.
    #[test]
    fn the_bind_path_and_the_dlsym_path_reach_the_same_function() {
        if !using_builtin() {
            return;
        }
        let out = published();
        let mut checked = 0;
        for (mach_o, address) in &out.functions {
            let bare = mach_o.strip_prefix('_').expect("the Mach-O underscore");
            // `symbol` stops at the first framework that answers, so this is
            // exactly the question "did the bind path and the dlsym path pick
            // the same winner", asked per name.
            let answered = symbol(bare).map(|a| format!("{a:#x}"));
            assert_eq!(
                symbol(bare),
                Some(*address),
                "{mach_o}: the flat provider binds {address:#x} and `dlsym` says {}",
                answered.as_deref().unwrap_or("nothing")
            );
            checked += 1;
        }
        assert!(checked > 100, "only {checked} functions checked");
    }
}
