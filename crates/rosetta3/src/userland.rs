use machish::error::Result;
use machish::image::MachObject;
use machish::namespace::{DylibRequest, DylibSource, FetchedDylib};
use machish::spec::arch::{Arm64, X86_64};

use crate::cxxlib::Flavor;

/// Install names answered from the base, as leaf names under `/usr/lib/`.
///
/// Each is a whole library family on a Mac - `libcurses` and `libtermcap` are
/// `libncurses` under older names, and `libreadline` is `libedit`'s
/// compatibility name - so all of a family is listed or none of it.
const LIBRARIES: &[&str] = &[
    "libutil.dylib",
    "libutil1.0.dylib",
    "libedit.dylib",
    "libedit.2.dylib",
    "libedit.3.dylib",
    "libedit.3.0.dylib",
    "libreadline.dylib",
    "libncurses.dylib",
    "libncurses.5.dylib",
    "libncurses.5.4.dylib",
    "libcurses.dylib",
    "libtermcap.dylib",
    "libssl.0.9.8.dylib",
    "libcrypto.0.9.8.dylib",
];

/// Whether `install_name` is one of the libraries this module takes from the base.
pub fn from_base(install_name: &str) -> bool {
    install_name
        .strip_prefix("/usr/lib/")
        .is_some_and(|leaf| LIBRARIES.contains(&leaf))
}

/// A [`DylibSource`] that answers [`LIBRARIES`] out of the current prefix, and
/// delegates everything else.
pub struct BaseLibraries<S> {
    inner: S,
    flavor: Flavor,
}

impl<S> BaseLibraries<S> {
    pub fn new(inner: S, flavor: Flavor) -> Self {
        Self { inner, flavor }
    }
}

impl<S: DylibSource> DylibSource for BaseLibraries<S> {
    fn fetch(&mut self, request: &DylibRequest<'_>) -> Result<Option<FetchedDylib>> {
        if !from_base(request.install_name) || prefix::ns::current().is_none() {
            return self.inner.fetch(request);
        }
        let path = prefix::ns::host_path(request.install_name);
        let Ok(bytes) = std::fs::read(&path) else {
            // A base without it: the providers answer, and an import nothing
            // defines is named at link time like any other.
            return self.inner.fetch(request);
        };
        // A base for the other architecture is not an error of the guest's,
        // and the providers are still a better answer than a failed link.
        if !self.flavor.can_load(&bytes) {
            return self.inner.fetch(request);
        }
        Ok(Some(FetchedDylib {
            path: path.to_string_lossy().into_owned(),
            bytes,
            implied: Vec::new(),
        }))
    }
}

impl Flavor {
    /// Whether `bytes` has a slice this flavor's backend can map.
    fn can_load(self, bytes: &[u8]) -> bool {
        match self {
            Flavor::Intel => MachObject::parse_any(bytes, X86_64).is_ok(),
            Flavor::Arm => MachObject::parse_any(bytes, Arm64).is_ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use machish::resolve::SymbolProvider;

    #[test]
    fn answers_for_userland_libraries_and_nothing_else() {
        assert!(from_base("/usr/lib/libutil.dylib"));
        assert!(from_base("/usr/lib/libutil1.0.dylib"));
        assert!(from_base("/usr/lib/libedit.3.dylib"));
        assert!(from_base("/usr/lib/libncurses.5.4.dylib"));
        assert!(from_base("/usr/lib/libreadline.dylib"));
        assert!(!from_base("/usr/lib/libSystem.B.dylib"));
        assert!(!from_base("@rpath/libedit.3.dylib"));
        assert!(!from_base("/usr/local/lib/libedit.3.dylib"));
        assert!(!darwin::provider().stands_for("/usr/lib/libutil.dylib"));
    }
}
