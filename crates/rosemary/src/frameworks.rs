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
        darwin::register_host_symbol_filter(|name| !crate::opengl::owns(name));
        register_dlopen_names();
        crate::dyld::install_image_hooks();
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
        leaf: "Network",
        version: 'A',
        umbrellas: &[],
        provide: Some(network::provide),
        data: None,
        symbol: Some(network::symbol),
    },
    Framework {
        leaf: "OpenCL",
        version: 'A',
        umbrellas: &[],
        provide: Some(opencl::provide),
        data: None,
        symbol: Some(opencl::symbol),
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
        leaf: "IOSurface",
        version: 'A',
        umbrellas: &[],
        provide: Some(iosurface::provide),
        data: Some(iosurface::data_symbols),
        symbol: Some(iosurface::symbol),
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
        crate::dyld::register_synthetic(&borrowed, dlsym_symbol);
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
    static NAMES: Vec<String> = std::env::var("ROSEMARY_WITHHOLD")
        .unwrap_or_default()
        .split(',')
        .map(|n| n.trim().trim_start_matches('_').to_string())
        .filter(|n| !n.is_empty())
        .collect();

    let bare = name.trim_start_matches('_');
    let out = NAMES.iter().any(|n| n == bare);
    if out {
        eprintln!("rosemary: {name}: withheld (ROSEMARY_WITHHOLD names it)");
    }
    out
}

pub fn provider() -> SyntheticDylib {
    install();
    let mut lib = SyntheticDylib::new("<rosemary frameworks>");
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
    // Pixel buffer attribute keys, which SDL's camera puts in an AVFoundation
    // settings dictionary.
    ("_kCVPixelBufferPixelFormatTypeKey", "PixelFormatType"),
    ("_kCVPixelBufferWidthKey", "Width"),
    ("_kCVPixelBufferHeightKey", "Height"),
];

fn media_constants() -> Vec<(String, u64)> {
    corefoundation::base::string_globals(MEDIA_CONSTANTS)
}

fn shimmed() -> Vec<(&'static str, *const ())> {
    vec![
        ("NSLog", crate::shims::nslog::native::nslog() as *const ()),
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
}

/// What `dlsym` on a provided framework answers: its functions, and then its
/// variables - `CFBundleGetDataPointerForName` asks for `kCFTypeArrayCallBacks`
/// this way, and has to be handed the address the guest's own bind got.
fn dlsym_symbol(name: &str) -> Option<u64> {
    symbol(name).or_else(|| data_symbol(name))
}

/// A variable the flat provider publishes, by its C name.
fn data_symbol(name: &str) -> Option<u64> {
    if !using_builtin() {
        return None;
    }
    // Built once: the tables allocate their objects on first use and answer
    // the same addresses after, which is what makes a cache of them correct.
    static DATA: std::sync::OnceLock<std::collections::HashMap<String, u64>> =
        std::sync::OnceLock::new();
    let data = DATA.get_or_init(|| {
        foundation::bootstrap();
        FRAMEWORKS
            .iter()
            .filter_map(|f| f.data)
            .flat_map(|data| data())
            .chain(media_constants())
            .collect()
    });
    let mangled = format!("_{name}");
    if withheld(&mangled) {
        return None;
    }
    data.get(&mangled).copied()
}

pub fn set_main_bundle(executable: &std::path::Path) {
    install();
    if !using_builtin() {
        return;
    }
    let found = foundation::bundle::set_main(executable);
    appkit::metadata::publish(executable);
    if !found && std::env::var_os("ROSEMARY_TRACE_VM").is_some() {
        eprintln!(
            "rosemary: bundle: {} is not inside a .app",
            executable.display()
        );
    }
}

/// Whether `ROSEMARY_NO_FOREIGN` forbids the host's own frameworks on macOS: neither
/// opened by a `dlopen` nor answering an import.
pub fn no_host_frameworks() -> bool {
    #[static_init::dynamic]
    static ON: bool =
        cfg!(target_os = "macos") && std::env::var_os("ROSEMARY_NO_FOREIGN").is_some();
    *ON
}

/// Whether `path` names a library inside a host framework directory.
pub fn is_host_framework_path(path: &str) -> bool {
    const DIRS: [&str; 3] = [
        "/System/Library/Frameworks/",
        "/System/Library/PrivateFrameworks/",
        "/Library/Frameworks/",
    ];
    DIRS.iter().any(|d| path.starts_with(d)) || path.contains(".framework/")
}

pub fn deny_host_frameworks(provider: &mut NativeDylibProvider) {
    install();
    if no_host_frameworks() {
        for dir in [
            "/System/Library/Frameworks/",
            "/System/Library/PrivateFrameworks/",
            "/Library/Frameworks/",
        ] {
            provider.deny_library_prefix(dir);
        }
    }
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
}
