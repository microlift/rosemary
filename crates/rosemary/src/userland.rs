use machish::error::Result;
use machish::namespace::{DylibRequest, DylibSource, FetchedDylib};

use crate::cxxlib::Flavor;

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
        if let Ok(lib) = self.inner.fetch(request)
            && lib.is_some()
        {
            return Ok(lib);
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
