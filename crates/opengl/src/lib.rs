//! OpenGL.framework: CGL, and the GL entry points of `libGL`. The framework's
//! other re-export, `libGLU`, is the Darwin base's own image ([`LIBGLU`]),
//! drawing through the GL answered here.
//!
//! A Mac program was only ever tested against Apple's GL - at most version 4.1,
//! the functions `libGL.dylib` exports, the extensions Apple's drivers list. A
//! host driver offers more, and a program that finds more takes paths no Mac
//! ran. So the framework answers exactly Apple's export list (see [`names`]),
//! each name bound to an implementation here or to the driver's function of the
//! same name, and reports Apple's version and extensions (see [`profile`]).
//! [`HOST_FEATURES`] lifts both limits.

pub mod cgl;
mod driver;
mod entry;
mod glsl;
mod names;
mod profile;

use names::Library;

/// The paths a guest names the framework by: the install names an
/// `LC_LOAD_DYLIB` records, and the spellings a `dlopen` uses.
pub const INSTALL_PATHS: &[&str] = &[
    "/System/Library/Frameworks/OpenGL.framework/Versions/A/OpenGL",
    "/System/Library/Frameworks/OpenGL.framework/Versions/Current/OpenGL",
    "/System/Library/Frameworks/OpenGL.framework/OpenGL",
    "/System/Library/Frameworks/OpenGL.framework/Versions/A/Libraries/libGL.dylib",
    "/System/Library/Frameworks/OpenGL.framework/Versions/Current/Libraries/libGL.dylib",
    "/System/Library/Frameworks/OpenGL.framework/Libraries/libGL.dylib",
    "/System/Library/Frameworks/OpenGL.framework/Versions/A/Libraries/libGLU.dylib",
    "/System/Library/Frameworks/OpenGL.framework/Versions/Current/Libraries/libGLU.dylib",
    "/System/Library/Frameworks/OpenGL.framework/Libraries/libGLU.dylib",
    "OpenGL",
];

/// The Darwin base's GLU, which the framework re-exports in place of Apple's
/// `Libraries/libGLU.dylib`.
pub const LIBGLU: &str = "/usr/lib/libGLU.1.dylib";

/// `ROSEMARY_GL_HOST_FEATURES`: offer the driver's own version, extensions and
/// entry points instead of a Mac's.
pub const HOST_FEATURES: &str = "ROSEMARY_GL_HOST_FEATURES";

/// Whether this run offers the driver's features rather than a Mac's.
pub fn host_features() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os(HOST_FEATURES).is_some_and(|value| !value.is_empty() && value != "0")
    })
}

/// Whether `name` belongs to this framework, answered or not: no host library
/// may stand in for it, as the host's GL is not the driver SDL made the
/// context against.
pub fn owns(name: &str) -> bool {
    name.starts_with("gl") || name.starts_with("CGL") || names::library(name).is_some()
}

/// The address a guest asking for the C name `name` should be given.
pub fn symbol(name: &str) -> Option<u64> {
    match names::library(name) {
        Some(Library::OpenGL) => cgl::symbol(name),
        Some(Library::Gl) => cgl::symbol(name).or_else(|| gl(name)),
        // Answered by [`LIBGLU`], never by this crate or the host.
        Some(Library::Glu) => None,
        None if host_features() && is_gl_function(name) => gl(name),
        None => None,
    }
}

fn gl(name: &str) -> Option<u64> {
    if !driver::open() {
        return None;
    }
    entry::address(name).or_else(|| driver::address(name))
}

/// `gl` and a capital: a GL function, as opposed to GLU's `glu`, CGL's private
/// `glc`, or GLX's `glX`, none of which a GL driver answers.
fn is_gl_function(name: &str) -> bool {
    name.strip_prefix("gl").is_some_and(|rest| {
        rest.starts_with(|c: char| c.is_ascii_uppercase()) && !rest.starts_with('X')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_gl_functions_reach_the_driver_beyond_the_list() {
        assert!(is_gl_function("glDispatchCompute"));
        assert!(!is_gl_function("gluNewQuadric"));
        assert!(!is_gl_function("glcNoop"));
        assert!(!is_gl_function("glXGetProcAddress"));
    }

    #[test]
    fn functions_a_mac_lacks_are_not_answered() {
        for name in ["glDispatchCompute", "glBufferStorage", "glCopyImageSubData"] {
            assert!(owns(name), "{name}");
            assert_eq!(symbol(name), None, "{name}");
        }
    }

    #[test]
    fn glu_is_left_to_the_base_image() {
        for name in ["gluOrtho2D", "gluPerspective"] {
            assert!(owns(name), "{name}");
            assert_eq!(symbol(name), None, "{name}");
        }
    }
}
