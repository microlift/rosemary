use machish::resolve::{LazyDylib, NativeDylibProvider, Resolved};

/// Built-in frameworks always draw through SDL's GL backend.
pub fn interposed() -> bool {
    crate::frameworks::using_builtin()
}

/// Names the framework answers or refuses, and no host library may.
pub fn owns(name: &str) -> bool {
    interposed() && opengl::owns(name)
}

/// The framework as a provider, for the link chains in [`crate::program`] and
/// [`crate::exec::native`].
///
/// Flat, and ahead of the host provider, so that a two-level import naming the
/// framework and a flat one naming nothing both land here. Lazy because that
/// placement would otherwise open a display connection and a GL driver in every
/// process, including the ones that never draw.
pub fn provider() -> LazyDylib {
    let mut lib = LazyDylib::new(opengl::INSTALL_PATHS[0], interposed, resolve);
    for name in &opengl::INSTALL_PATHS[1..] {
        lib.alias(name);
    }
    // Claims are backend-dependent even for explicitly named imports. Driver
    // loading stays in `opengl::symbol`, so CGL needs no graphics
    // initialization.
    lib.flat(interposed())
        .claims(|symbol| symbol.strip_prefix('_').is_some_and(owns));
    lib
}

/// Keep the host's GL out of a guest that has SDL's.
///
/// The failure this exists to stop is not a missing symbol: the host loader has
/// a `libGL.so.1` of its own and answers every `gl` name from it, so the guest
/// links, runs, and draws through a driver that never saw the context SDL made.
/// What that looks like is a blank window, a GL error that names nothing, or a
/// crash inside the driver - never a report that two libraries were mixed.
pub fn deny_host_gl(provider: &mut NativeDylibProvider) {
    if interposed() {
        provider.deny_prefix("_gl");
        provider.deny_prefix("_CGL");
    }
}

/// The address a `dlsym` of the C name `name` should answer, for
/// [`crate::frameworks`].
pub fn symbol(name: &str) -> Option<u64> {
    interposed().then(|| opengl::symbol(name)).flatten()
}

fn resolve(name: &str) -> Option<Resolved> {
    symbol(name.strip_prefix('_')?).map(Resolved::function)
}

#[cfg(test)]
mod tests {
    use super::*;
    use machish::fixups::{LibraryRef, SymbolRef};
    use machish::resolve::SymbolProvider;

    fn request(mangled: &str) -> SymbolRef<'_> {
        SymbolRef {
            name: mangled,
            library: LibraryRef::InstallName(opengl::INSTALL_PATHS[0]),
            weak_import: false,
        }
    }

    #[test]
    fn backend_selection_child() {
        let Ok(mode) = std::env::var("ROSEMARY_GL_TEST_CHILD") else {
            return;
        };
        let builtin = mode == "builtin";
        assert_eq!(interposed(), builtin);
        let lib = provider();
        let mut host = NativeDylibProvider::system();
        deny_host_gl(&mut host);
        assert_eq!(host.is_denied("_CGLChoosePixelFormat"), builtin);
        assert_eq!(host.is_denied("_glClear"), builtin);
        crate::frameworks::provider();
        for name in [
            "CGLChoosePixelFormat",
            "CGLGetCurrentContext",
            "CGLFlushDrawable",
            "CGLGetShareGroup",
        ] {
            let mangled = format!("_{name}");
            if builtin {
                let expected = opengl::cgl::symbol(name).unwrap();
                assert_eq!(symbol(name), Some(expected));
                assert_eq!(lib.resolve(&request(&mangled)).unwrap().addr, expected);
                assert_eq!(darwin::symbol(name), Some(expected));
                assert_eq!(darwin::shim_symbol(name), Some(expected));
            } else {
                assert!(symbol(name).is_none());
                assert!(lib.resolve(&request(&mangled)).is_none());
            }
        }
        if builtin {
            // Unimplemented, and a function a Mac's framework does not export:
            // neither may fall through to the host.
            for name in [
                "CGLDescribePixelFormat",
                "gluPerspective",
                "glDispatchCompute",
            ] {
                assert!(symbol(name).is_none(), "{name}");
                assert!(darwin::symbol(name).is_none(), "{name}");
                assert!(darwin::shim_symbol(name).is_none(), "{name}");
            }
        } else {
            assert_eq!(lib.opened(), None);
        }
    }

    #[test]
    fn backend_selection_controls_imports_and_fallbacks() {
        let modes: &[&str] = if cfg!(target_os = "macos") {
            &["native", "builtin"]
        } else {
            &["builtin"]
        };
        for mode in modes {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "gl::tests::backend_selection_child",
                    "--nocapture",
                ])
                .env("ROSEMARY_OBJC", mode)
                .env("ROSEMARY_GL_TEST_CHILD", mode)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
