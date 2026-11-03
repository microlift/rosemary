mod lazy;
mod synthetic;

#[cfg(feature = "std")]
mod native;

pub use lazy::LazyDylib;
pub use synthetic::SyntheticDylib;

#[cfg(feature = "std")]
pub use native::NativeDylibProvider;

use alloc::string::String;
use alloc::vec::Vec;

use crate::fixups::{LibraryRef, SymbolRef};
use crate::raw::Word;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum SymbolKind {
    #[default]
    Function,
    Data,
    ThreadLocal,
    /// Deliberately not present. Distinct from an unresolved symbol: this is a
    /// provider saying "this name exists and its address is zero".
    Absent,
}

/// A provider's answer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct Resolved {
    /// The address to store in the slot. For high-level emulation this is a
    /// host pointer, valid in the host's address space, which works only
    /// because the guest image is identity-mapped.
    pub addr: Word,
    pub kind: SymbolKind,
    /// Opaque, provider-defined, copied verbatim into [`crate::report::BindingRecord::tag`].
    pub tag: u64,
}

impl Resolved {
    /// A function at `addr`, untagged.
    pub fn function(addr: Word) -> Self {
        Resolved {
            addr,
            kind: SymbolKind::Function,
            tag: 0,
        }
    }

    /// A datum at `addr`, untagged.
    pub fn data(addr: Word) -> Self {
        Resolved {
            addr,
            kind: SymbolKind::Data,
            tag: 0,
        }
    }

    /// Attach a provider-defined tag.
    pub fn with_tag(mut self, tag: u64) -> Self {
        self.tag = tag;
        self
    }
}

/// Something that can say what an import resolves to.
pub trait SymbolProvider {
    /// Whether this provider stands for the image called `install_name`.
    ///
    /// Consulted before [`resolve`](Self::resolve) for a two-level reference,
    /// and skipped entirely for a flat one. Matching should accept the whole
    /// install name as recorded (`/usr/lib/libSystem.B.dylib`); see
    /// [`SyntheticDylib::alias`] for the umbrella case, where one provider
    /// legitimately stands for several names.
    fn provides(&self, install_name: &str) -> bool;

    /// Whether this provider **is** the image called `install_name`, so a
    /// dependency on it is already satisfied and the loader fetches nothing.
    ///
    /// Stricter than [`provides`](Self::provides), which a flat provider answers
    /// for every name: standing in for a library is a claim about its whole
    /// surface, and a flat provider that happens to define `_strlen` has not
    /// made one.
    fn stands_for(&self, _install_name: &str) -> bool {
        false
    }

    /// The real images this provider's stand-in for `install_name` re-exports,
    /// as `LC_REEXPORT_DYLIB` would name them: the loader fetches them in its
    /// place, and a two-level import of `install_name` this provider does not
    /// define is looked for in them next.
    ///
    /// Empty unless [`stands_for`](Self::stands_for) is true for the name.
    fn reexports(&self, _install_name: &str) -> Vec<String> {
        Vec::new()
    }

    /// The address `sym` names, or `None` if this provider does not define it.
    fn resolve(&self, sym: &SymbolRef<'_>) -> Option<Resolved>;

    /// Offer every symbol this provider knows, for diagnostics. Providers
    /// backed by `dlsym` cannot enumerate and correctly do nothing here.
    fn enumerate(&self, _out: &mut dyn FnMut(&str, Resolved)) {}
}

pub fn eager_binding_shims() -> SyntheticDylib {
    let mut lib = SyntheticDylib::new("<machish eager-binding shims>");
    lib.flat(true).define_absent("dyld_stub_binder");
    lib
}

/// Resolve `sym` against `providers`, honouring two-level scoping.
///
/// A two-level reference consults only providers that claim its library; a flat
/// or special reference consults all of them in order, first match winning.
pub fn resolve_in(providers: &[&dyn SymbolProvider], sym: &SymbolRef<'_>) -> Option<Resolved> {
    resolve_at_depth(providers, sym, 0)
}

/// How many stand-in re-export hops to follow before giving up on a cycle.
const MAX_REEXPORT_DEPTH: usize = 16;

fn resolve_at_depth(
    providers: &[&dyn SymbolProvider],
    sym: &SymbolRef<'_>,
    depth: usize,
) -> Option<Resolved> {
    match sym.library {
        LibraryRef::InstallName(name) => {
            providers.iter().filter(|p| p.provides(name)).find_map(|p| {
                p.resolve(sym).or_else(|| {
                    // Where the stand-in sits in the chain, as dyld searches an
                    // umbrella's re-exports right after the umbrella itself.
                    if depth >= MAX_REEXPORT_DEPTH {
                        return None;
                    }
                    p.reexports(name).iter().find_map(|target| {
                        let forwarded = SymbolRef {
                            library: LibraryRef::InstallName(target),
                            ..*sym
                        };
                        resolve_at_depth(providers, &forwarded, depth + 1)
                    })
                })
            })
        }
        // Everything else - flat lookup, weak lookup, an ordinal we could not
        // match to a load command - is a search across all providers. For an
        // unknown ordinal that is a deliberate softening: the alternative is
        // failing an import over a malformed number when the name alone is
        // very likely enough to find it.
        _ => providers.iter().find_map(|p| p.resolve(sym)),
    }
}
