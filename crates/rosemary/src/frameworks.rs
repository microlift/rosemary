use exports::Exports;
use machish::resolve::{NativeDylibProvider, SyntheticDylib};

use crate::dyld::Links;
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
        darwin::register_host_symbol_filter(|name| !crate::gl::owns(name));
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
    links: Links,
}

/// The install name of the framework `leaf` names.
macro_rules! framework {
    ($leaf:literal) => {
        concat!("/System/Library/Frameworks/", $leaf, ".framework/", $leaf)
    };
}

const LIBSYSTEM: &str = "/usr/lib/libSystem.B.dylib";
const LIBOBJC: &str = "/usr/lib/libobjc.A.dylib";
const LIBOBJC_NAMES: &[&str] = &[LIBOBJC, "/usr/lib/libobjc.dylib"];

// FIXME (ACCURACY): each row's `links` is a recollection of that framework's
// `otool -L` on macOS, cut to libraries this runtime provides, and not checked
// against a real install. A row standing for several frameworks - AppKit with
// CoreGraphics - answers for all of them wherever any one is linked.

static FRAMEWORKS: &[Framework] = &[
    Framework {
        leaf: "CoreFoundation",
        version: 'A',
        umbrellas: &[],
        provide: Some(corefoundation::provide),
        data: Some(corefoundation_data),
        symbol: Some(corefoundation::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[LIBOBJC, LIBSYSTEM],
        },
    },
    Framework {
        leaf: "Foundation",
        version: 'C',
        umbrellas: &[],
        provide: Some(foundation::provide),
        data: Some(foundation::data_symbols),
        symbol: Some(foundation::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreFoundation"),
                LIBOBJC,
                framework!("Security"),
                framework!("IOKit"),
                framework!("CoreServices"),
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "IOKit",
        version: 'A',
        umbrellas: &[],
        provide: Some(iokit::provide),
        data: Some(iokit::data_symbols),
        symbol: Some(iokit::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[framework!("CoreFoundation"), LIBSYSTEM],
        },
    },
    Framework {
        leaf: "ForceFeedback",
        version: 'A',
        umbrellas: &[],
        provide: Some(forcefeedback::provide),
        data: None,
        symbol: Some(forcefeedback::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[framework!("IOKit"), framework!("CoreFoundation"), LIBSYSTEM],
        },
    },
    Framework {
        leaf: "Network",
        version: 'A',
        umbrellas: &[],
        provide: Some(network::provide),
        data: None,
        symbol: Some(network::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreFoundation"),
                framework!("Security"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "OpenCL",
        version: 'A',
        umbrellas: &[],
        provide: Some(opencl::provide),
        data: None,
        symbol: Some(opencl::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreFoundation"),
                framework!("IOSurface"),
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "AppKit",
        version: 'C',
        // AppKit's crate also implements Quartz, so a `dlopen` of
        // CoreGraphics by path has to find these too.
        umbrellas: &["Cocoa", "CoreGraphics"],
        provide: Some(appkit::provide),
        data: Some(appkit::data_symbols),
        symbol: Some(appkit::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("Foundation"),
                framework!("CoreFoundation"),
                framework!("ApplicationServices"),
                framework!("CoreText"),
                framework!("CoreServices"),
                framework!("IOKit"),
                framework!("IOSurface"),
                framework!("CoreVideo"),
                framework!("Metal"),
                framework!("Security"),
                framework!("UniformTypeIdentifiers"),
                framework!("OpenGL"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "CoreText",
        version: 'A',
        umbrellas: &[],
        provide: Some(coretext::provide),
        data: Some(coretext::data_symbols),
        symbol: Some(coretext::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreFoundation"),
                framework!("CoreGraphics"),
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "ApplicationServices",
        version: 'A',
        umbrellas: &[],
        provide: Some(applicationservices::provide),
        data: None,
        symbol: Some(applicationservices::symbol),
        links: Links {
            reexports: &[framework!("CoreGraphics"), framework!("CoreText")],
            dependencies: &[
                framework!("CoreFoundation"),
                framework!("CoreServices"),
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "CoreServices",
        version: 'A',
        umbrellas: &["Carbon"],
        provide: Some(coreservices::provide),
        data: None,
        symbol: Some(coreservices::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreFoundation"),
                framework!("IOKit"),
                framework!("Security"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "AudioToolbox",
        version: 'A',
        umbrellas: &[],
        provide: Some(audiotoolbox::provide),
        data: None,
        symbol: Some(audiotoolbox::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreAudio"),
                framework!("CoreFoundation"),
                framework!("Foundation"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "CoreMedia",
        version: 'A',
        umbrellas: &[],
        provide: Some(coremedia::provide),
        data: Some(coremedia_data),
        symbol: Some(coremedia::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreFoundation"),
                framework!("CoreAudio"),
                framework!("CoreVideo"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "CoreVideo",
        version: 'A',
        umbrellas: &[],
        provide: Some(corevideo::provide),
        data: Some(corevideo_data),
        symbol: Some(corevideo::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreFoundation"),
                framework!("IOSurface"),
                framework!("Metal"),
                framework!("CoreGraphics"),
                framework!("OpenGL"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "VideoToolbox",
        version: 'A',
        umbrellas: &[],
        provide: Some(videotoolbox::provide),
        data: Some(videotoolbox::data_symbols),
        symbol: Some(videotoolbox::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreMedia"),
                framework!("CoreVideo"),
                framework!("CoreFoundation"),
                framework!("IOSurface"),
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "IOSurface",
        version: 'A',
        umbrellas: &[],
        provide: Some(iosurface::provide),
        data: Some(iosurface::data_symbols),
        symbol: Some(iosurface::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("IOKit"),
                framework!("CoreFoundation"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "CoreAudio",
        version: 'A',
        umbrellas: &[],
        provide: Some(coreaudio::provide),
        data: None,
        symbol: Some(coreaudio::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreFoundation"),
                framework!("IOKit"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "UniformTypeIdentifiers",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(coreservices::uttype::data_symbols),
        symbol: None,
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("Foundation"),
                framework!("CoreFoundation"),
                framework!("CoreServices"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "Contacts",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(contacts::data_symbols),
        symbol: None,
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("Foundation"),
                framework!("CoreFoundation"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "AVFoundation",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(avfoundation_data),
        symbol: None,
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreMedia"),
                framework!("CoreVideo"),
                framework!("CoreAudio"),
                framework!("AudioToolbox"),
                framework!("Foundation"),
                framework!("CoreFoundation"),
                framework!("IOSurface"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "Security",
        version: 'A',
        umbrellas: &[],
        provide: Some(security::provide),
        data: Some(security::data_symbols),
        symbol: Some(security::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreFoundation"),
                framework!("Foundation"),
                framework!("IOKit"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "GameController",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(gamecontroller::data_symbols),
        symbol: Some(gamecontroller::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("Foundation"),
                framework!("CoreFoundation"),
                framework!("IOKit"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "WebKit",
        version: 'A',
        umbrellas: &[],
        provide: None,
        data: Some(webkit::data_symbols),
        symbol: Some(webkit::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("AppKit"),
                framework!("Foundation"),
                framework!("CoreFoundation"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
    },
    Framework {
        leaf: "Metal",
        version: 'A',
        umbrellas: &[],
        provide: Some(metal::provide),
        data: Some(metal::data_symbols),
        symbol: Some(metal::symbol),
        links: Links {
            reexports: &[],
            dependencies: &[
                framework!("Foundation"),
                framework!("CoreFoundation"),
                framework!("IOSurface"),
                framework!("IOKit"),
                LIBOBJC,
                LIBSYSTEM,
            ],
        },
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
    for (row, framework) in FRAMEWORKS.iter().enumerate() {
        let paths = paths_of(framework);
        let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();
        crate::dyld::register_synthetic(
            &borrowed,
            move |name| framework_symbol(row, name),
            framework.links,
        );
    }
    register_opengl_framework();
    register_openal_framework();
    register_system_libraries();
}

fn register_system_libraries() {
    crate::dyld::register_synthetic(
        darwin::LIBRARY_NAMES,
        libsystem_symbol,
        Links {
            reexports: darwin::reexports(),
            dependencies: &[],
        },
    );
    crate::dyld::register_synthetic(
        LIBOBJC_NAMES,
        crate::objc::host::symbol,
        Links {
            reexports: &[],
            dependencies: &[LIBSYSTEM],
        },
    );
}

/// What libSystem's own image exports: `darwin`, and libclosure, which is
/// `libsystem_blocks` and which nothing on a host off Darwin defines.
fn libsystem_symbol(name: &str) -> Option<u64> {
    crate::dyld::libsystem_symbol(name).or_else(|| objc::block::symbol(name))
}

fn register_opengl_framework() {
    if !crate::gl::interposed() {
        return;
    }
    crate::dyld::register_synthetic(
        opengl::INSTALL_PATHS,
        crate::gl::symbol,
        Links {
            reexports: &[],
            dependencies: &[framework!("CoreFoundation"), framework!("IOKit"), LIBSYSTEM],
        },
    );
}

fn register_openal_framework() {
    if !crate::openal::interposed() {
        return;
    }
    crate::dyld::register_synthetic(
        crate::openal::INSTALL_PATHS,
        crate::openal::symbol,
        Links {
            reexports: &[],
            dependencies: &[
                framework!("CoreAudio"),
                framework!("AudioToolbox"),
                framework!("CoreFoundation"),
                LIBSYSTEM,
            ],
        },
    );
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
    for (name, addr) in shimmed() {
        out.function(&format!("_{name}"), addr as usize as u64);
    }
    lib
}

/// AVFoundation's string constants.
const AV_CONSTANTS: &[(&str, &str)] = &[
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

/// CoreVideo's pixel buffer, attachment and pool keys. The texts are Apple's,
/// as a guest may compare against a CFSTR.
const CV_CONSTANTS: &[(&str, &str)] = &[
    ("_kCVPixelBufferPixelFormatTypeKey", "PixelFormatType"),
    ("_kCVPixelBufferWidthKey", "Width"),
    ("_kCVPixelBufferHeightKey", "Height"),
    ("_kCVPixelBufferMemoryAllocatorKey", "MemoryAllocator"),
    (
        "_kCVPixelBufferBytesPerRowAlignmentKey",
        "BytesPerRowAlignment",
    ),
    ("_kCVPixelBufferPlaneAlignmentKey", "PlaneAlignment"),
    (
        "_kCVPixelBufferIOSurfacePropertiesKey",
        "IOSurfaceProperties",
    ),
    (
        "_kCVPixelBufferOpenGLCompatibilityKey",
        "OpenGLCompatibility",
    ),
    ("_kCVPixelBufferMetalCompatibilityKey", "MetalCompatibility"),
    (
        "_kCVPixelBufferOpenGLESCompatibilityKey",
        "OpenGLESCompatibility",
    ),
    (
        "_kCVPixelBufferOpenGLESTextureCacheCompatibilityKey",
        "OpenGLESTextureCacheCompatibility",
    ),
    (
        "_kCVPixelBufferIOSurfaceCoreAnimationCompatibilityKey",
        "IOSurfaceCoreAnimationCompatibility",
    ),
    (
        "_kCVPixelBufferIOSurfaceOpenGLFBOCompatibilityKey",
        "IOSurfaceOpenGLFBOCompatibility",
    ),
    (
        "_kCVPixelBufferIOSurfaceOpenGLESFBOCompatibilityKey",
        "IOSurfaceOpenGLESFBOCompatibility",
    ),
    (
        "_kCVPixelBufferIOSurfaceOpenGLTextureCompatibilityKey",
        "IOSurfaceOpenGLTextureCompatibility",
    ),
    (
        "_kCVPixelBufferIOSurfaceOpenGLESTextureCompatibilityKey",
        "IOSurfaceOpenGLESTextureCompatibility",
    ),
    ("_kCVPixelBufferIOSurfacePurgeableKey", "IOSurfacePurgeable"),
    (
        "_kCVPixelBufferOpenGLTextureCacheCompatibilityKey",
        "OpenGLTextureCacheCompatibility",
    ),
    (
        "_kCVPixelBufferCGBitmapContextCompatibilityKey",
        "CGBitmapContextCompatibility",
    ),
    (
        "_kCVPixelBufferCGImageCompatibilityKey",
        "CGImageCompatibility",
    ),
    ("_kCVPixelBufferExtendedPixelsLeftKey", "ExtendedPixelsLeft"),
    ("_kCVPixelBufferExtendedPixelsTopKey", "ExtendedPixelsTop"),
    (
        "_kCVPixelBufferExtendedPixelsRightKey",
        "ExtendedPixelsRight",
    ),
    (
        "_kCVPixelBufferExtendedPixelsBottomKey",
        "ExtendedPixelsBottom",
    ),
    (
        "_kCVBufferPropagatedAttachmentsKey",
        "PropagatedAttachments",
    ),
    (
        "_kCVBufferNonPropagatedAttachmentsKey",
        "NonPropagatedAttachments",
    ),
    (
        "_kCVImageBufferCGColorSpaceKey",
        "CVImageBufferCGColorSpace",
    ),
    ("_kCVImageBufferCleanApertureKey", "CVCleanAperture"),
    ("_kCVImageBufferPixelAspectRatioKey", "CVPixelAspectRatio"),
    (
        "_kCVImageBufferPixelAspectRatioHorizontalSpacingKey",
        "HorizontalSpacing",
    ),
    (
        "_kCVImageBufferPixelAspectRatioVerticalSpacingKey",
        "VerticalSpacing",
    ),
    ("_kCVImageBufferFieldCountKey", "CVFieldCount"),
    ("_kCVImageBufferFieldDetailKey", "CVFieldDetail"),
    ("_kCVImageBufferYCbCrMatrixKey", "CVImageBufferYCbCrMatrix"),
    ("_kCVImageBufferYCbCrMatrix_ITU_R_709_2", "ITU_R_709_2"),
    ("_kCVImageBufferYCbCrMatrix_ITU_R_601_4", "ITU_R_601_4"),
    (
        "_kCVImageBufferYCbCrMatrix_SMPTE_240M_1995",
        "SMPTE_240M_1995",
    ),
    ("_kCVImageBufferYCbCrMatrix_ITU_R_2020", "ITU_R_2020"),
    (
        "_kCVImageBufferColorPrimariesKey",
        "CVImageBufferColorPrimaries",
    ),
    ("_kCVImageBufferColorPrimaries_ITU_R_709_2", "ITU_R_709_2"),
    ("_kCVImageBufferColorPrimaries_EBU_3213", "EBU_3213"),
    ("_kCVImageBufferColorPrimaries_SMPTE_C", "SMPTE_C"),
    ("_kCVImageBufferColorPrimaries_ITU_R_2020", "ITU_R_2020"),
    (
        "_kCVImageBufferTransferFunctionKey",
        "CVImageBufferTransferFunction",
    ),
    ("_kCVImageBufferTransferFunction_ITU_R_709_2", "ITU_R_709_2"),
    (
        "_kCVImageBufferTransferFunction_SMPTE_240M_1995",
        "SMPTE_240M_1995",
    ),
    ("_kCVImageBufferTransferFunction_ITU_R_2020", "ITU_R_2020"),
    (
        "_kCVImageBufferChromaLocationTopFieldKey",
        "CVImageBufferChromaLocationTopField",
    ),
    (
        "_kCVImageBufferChromaLocationBottomFieldKey",
        "CVImageBufferChromaLocationBottomField",
    ),
    ("_kCVImageBufferChromaLocation_Left", "Left"),
    ("_kCVImageBufferChromaLocation_Center", "Center"),
    ("_kCVImageBufferChromaLocation_TopLeft", "TopLeft"),
    ("_kCVImageBufferChromaLocation_Top", "Top"),
    ("_kCVImageBufferChromaLocation_BottomLeft", "BottomLeft"),
    ("_kCVImageBufferChromaLocation_Bottom", "Bottom"),
    (
        "_kCVPixelBufferPoolMinimumBufferCountKey",
        "MinimumBufferCount",
    ),
    ("_kCVPixelBufferPoolMaximumBufferAgeKey", "MaximumBufferAge"),
    (
        "_kCVPixelBufferPoolAllocationThresholdKey",
        "kCVPixelBufferPoolAllocationThresholdKey",
    ),
];

/// CoreFoundation's `CFUserNotification` keys.
const CF_USER_NOTIFICATION_CONSTANTS: &[(&str, &str)] = &[
    ("_kCFUserNotificationAlertHeaderKey", "AlertHeader"),
    ("_kCFUserNotificationAlertMessageKey", "AlertMessage"),
    (
        "_kCFUserNotificationDefaultButtonTitleKey",
        "DefaultButtonTitle",
    ),
];

/// `CMTimeFlags`, from CMTime.h.
const CM_TIME_VALID: u32 = 1;
const CM_TIME_POSITIVE_INFINITY: u32 = 1 << 2;
const CM_TIME_NEGATIVE_INFINITY: u32 = 1 << 3;

/// `(symbol, value, timescale, flags)` for the `CMTime` globals, which are
/// structs rather than objects: the symbol is the struct's own address.
const CM_TIME_CONSTANTS: &[(&str, i64, i32, u32)] = &[
    ("_kCMTimeInvalid", 0, 0, 0),
    ("_kCMTimeZero", 0, 1, CM_TIME_VALID),
    (
        "_kCMTimePositiveInfinity",
        0,
        0,
        CM_TIME_VALID | CM_TIME_POSITIVE_INFINITY,
    ),
    (
        "_kCMTimeNegativeInfinity",
        0,
        0,
        CM_TIME_VALID | CM_TIME_NEGATIVE_INFINITY,
    ),
];

fn cm_time_constants() -> Vec<(String, u64)> {
    // Built once, so the same call answers the same addresses.
    static TIMES: std::sync::OnceLock<Vec<(String, u64)>> = std::sync::OnceLock::new();
    TIMES
        .get_or_init(|| {
            CM_TIME_CONSTANTS
                .iter()
                .map(|&(symbol, value, timescale, flags)| {
                    // Leaked: the guest holds the address for the life of the process.
                    let words: &'static mut [u64; 3] = Box::leak(Box::new([
                        value as u64,
                        u64::from(timescale as u32) | u64::from(flags) << 32,
                        0,
                    ]));
                    (symbol.to_string(), words.as_ptr() as u64)
                })
                .collect()
        })
        .clone()
}

fn corefoundation_data() -> Vec<(String, u64)> {
    let mut out = corefoundation::data_symbols();
    out.extend(corefoundation::base::string_globals(
        CF_USER_NOTIFICATION_CONSTANTS,
    ));
    out
}

fn coremedia_data() -> Vec<(String, u64)> {
    let mut out = coremedia::data_symbols();
    out.extend(cm_time_constants());
    out
}

fn corevideo_data() -> Vec<(String, u64)> {
    corefoundation::base::string_globals(CV_CONSTANTS)
}

fn avfoundation_data() -> Vec<(String, u64)> {
    let mut out = avfoundation::data_symbols();
    out.extend(corefoundation::base::string_globals(AV_CONSTANTS));
    out
}

fn shimmed() -> Vec<(&'static str, *const ())> {
    vec![
        ("NSLog", crate::shims::nslog::native::nslog() as *const ()),
        (
            "NSRunAlertPanel",
            crate::shims::nslog::native::run_alert_panel() as *const (),
        ),
        (
            "CFStringCreateWithFormat",
            crate::shims::nslog::native::cfstring_create_with_format() as *const (),
        ),
        (
            "CFStringCreateWithFormatAndArguments",
            crate::shims::nslog::native::cfstring_create_with_format_and_arguments() as *const (),
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
        // OpenGL, whose driver half is reached by `dlsym` rather than
        // enumerated - see `crate::gl`, which publishes it lazily.
        .or_else(|| crate::gl::symbol(name))
        .or_else(|| crate::openal::symbol(name))
}

/// What `dlsym` on the framework at `row` answers: its functions, and then its
/// variables - `CFBundleGetDataPointerForName` asks for `kCFTypeArrayCallBacks`
/// this way, and has to be handed the address the guest's own bind got.
fn framework_symbol(row: usize, name: &str) -> Option<u64> {
    foundation::bootstrap();
    FRAMEWORKS[row]
        .symbol
        .and_then(|symbol| symbol(name))
        .or_else(|| data_symbol(row, name))
}

/// A variable the flat provider publishes for the framework at `row`, by its C
/// name.
fn data_symbol(row: usize, name: &str) -> Option<u64> {
    // Built once: the tables allocate their objects on first use and answer
    // the same addresses after, which is what makes a cache of them correct.
    static DATA: std::sync::OnceLock<std::collections::HashMap<String, (usize, u64)>> =
        std::sync::OnceLock::new();
    let data = DATA.get_or_init(|| {
        foundation::bootstrap();
        FRAMEWORKS
            .iter()
            .enumerate()
            .filter_map(|(row, f)| Some((row, f.data?)))
            .flat_map(|(row, data)| data().into_iter().map(move |(n, a)| (n, (row, a))))
            .collect()
    });
    let mangled = format!("_{name}");
    if withheld(&mangled) {
        return None;
    }
    data.get(&mangled)
        .filter(|(owner, _)| *owner == row)
        .map(|&(_, addr)| addr)
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
