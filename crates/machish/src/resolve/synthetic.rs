use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::fixups::SymbolRef;
use crate::raw::Word;
use crate::resolve::{Resolved, SymbolKind, SymbolProvider};

/// A hand-populated stand-in for a real dylib.
///
/// ```
/// use machish::resolve::SyntheticDylib;
///
/// extern "C" fn my_strlen(_: *const u8) -> usize { 0 }
///
/// let mut libc = SyntheticDylib::new("/usr/lib/libSystem.B.dylib");
/// libc.alias("/usr/lib/system/libsystem_c.dylib");
/// libc.define_fn("_strlen", my_strlen as *const ());
/// ```
#[derive(Clone, Debug)]
pub struct SyntheticDylib {
    install_name: String,
    aliases: Vec<String>,
    reexports: Vec<String>,
    flat: bool,
    symbols: BTreeMap<String, Resolved>,
}

impl SyntheticDylib {
    /// An empty library standing for `install_name`.
    pub fn new(install_name: &str) -> Self {
        SyntheticDylib {
            install_name: install_name.to_string(),
            aliases: Vec::new(),
            reexports: Vec::new(),
            flat: false,
            symbols: BTreeMap::new(),
        }
    }

    pub fn install_name(&self) -> &str {
        &self.install_name
    }

    /// Also answer to `install_name`, for umbrellas and re-export chains.
    pub fn alias(&mut self, install_name: &str) -> &mut Self {
        self.aliases.push(install_name.to_string());
        self
    }

    /// Re-export the real image `install_name`, as the library this stands
    /// for does: the loader fetches it, and what this does not define is
    /// looked for there. See [`SymbolProvider::reexports`].
    pub fn reexport(&mut self, install_name: &str) -> &mut Self {
        self.reexports.push(install_name.to_string());
        self
    }

    /// Whether to answer regardless of which library an import names.
    ///
    /// A flat provider placed first in the chain shadows every two-level
    /// binding of the names it defines, which is the point. It is how a symbol
    /// that must be virtualized is guaranteed not to reach the host.
    pub fn flat(&mut self, flat: bool) -> &mut Self {
        self.flat = flat;
        self
    }

    /// Define `symbol` - leading underscore included, as the image spells it.
    pub fn define(&mut self, symbol: &str, resolved: Resolved) -> &mut Self {
        self.symbols.insert(symbol.to_string(), resolved);
        self
    }

    /// Define `symbol` as a function at a host pointer.
    pub fn define_fn(&mut self, symbol: &str, ptr: *const ()) -> &mut Self {
        self.define(symbol, Resolved::function(ptr as usize as Word))
    }

    /// Define `symbol` as data at `addr`.
    pub fn define_data(&mut self, symbol: &str, addr: Word) -> &mut Self {
        self.define(symbol, Resolved::data(addr))
    }

    /// Define `symbol` as deliberately absent, so a weak import binds to zero
    /// here rather than falling through to a later provider that would
    /// wrongly satisfy it.
    pub fn define_absent(&mut self, symbol: &str) -> &mut Self {
        self.define(
            symbol,
            Resolved {
                addr: 0,
                kind: SymbolKind::Absent,
                tag: 0,
            },
        )
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }
}

impl SymbolProvider for SyntheticDylib {
    fn provides(&self, install_name: &str) -> bool {
        self.flat || self.stands_for(install_name)
    }

    fn stands_for(&self, install_name: &str) -> bool {
        self.install_name == install_name || self.aliases.iter().any(|a| a == install_name)
    }

    fn reexports(&self, install_name: &str) -> Vec<String> {
        match self.stands_for(install_name) {
            true => self.reexports.clone(),
            false => Vec::new(),
        }
    }

    fn resolve(&self, sym: &SymbolRef<'_>) -> Option<Resolved> {
        if let Some(exact) = self.symbols.get(sym.name) {
            return Some(*exact);
        }
        // A `$` suffix marks a platform variant of the same function:
        // `_close$NOCANCEL`, `_fopen$DARWIN_EXTSN`, `_fts_open$INODE64`,
        // `_dispatch_queue_create_with_target$V2`. The variant is a promise about
        // the *base* symbol's behaviour - not a cancellation point, the extended
        // return convention, 64-bit inodes - and a definition of the base name is
        // the implementation of all of them. Without this a guest importing the
        // suffixed spelling binds nothing at all, which faults at the call rather
        // than diverging in behaviour.
        let (base, variant) = sym.name.split_once('$')?;
        if base.is_empty() || variant.is_empty() {
            return None;
        }
        self.symbols.get(base).copied()
    }

    fn enumerate(&self, out: &mut dyn FnMut(&str, Resolved)) {
        for (name, r) in &self.symbols {
            out(name, *r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixups::LibraryRef;
    use crate::resolve::resolve_in;

    fn sym<'a>(name: &'a str, library: LibraryRef<'a>) -> SymbolRef<'a> {
        SymbolRef {
            name,
            library,
            weak_import: false,
        }
    }

    #[test]
    fn two_level_lookup_respects_the_install_name() {
        let mut lib = SyntheticDylib::new("/usr/lib/libSystem.B.dylib");
        lib.define_fn("_write", 0x1234 as *const ());
        let providers: [&dyn SymbolProvider; 1] = [&lib];

        let found = resolve_in(
            &providers,
            &sym(
                "_write",
                LibraryRef::InstallName("/usr/lib/libSystem.B.dylib"),
            ),
        );
        assert_eq!(found.map(|r| r.addr), Some(0x1234));

        // The whole point of two-level: the same name from a different library
        // is a different binding and must not be satisfied here.
        let other = resolve_in(
            &providers,
            &sym("_write", LibraryRef::InstallName("@rpath/libmine.dylib")),
        );
        assert_eq!(other, None);
    }

    #[test]
    fn a_flat_reference_ignores_the_install_name() {
        let mut lib = SyntheticDylib::new("/usr/lib/libSystem.B.dylib");
        lib.define_fn("_write", 0x1234 as *const ());
        let providers: [&dyn SymbolProvider; 1] = [&lib];

        assert!(resolve_in(&providers, &sym("_write", LibraryRef::Flat)).is_some());
    }

    #[test]
    fn aliases_and_flat_mode_widen_matching() {
        let mut umbrella = SyntheticDylib::new("/usr/lib/libSystem.B.dylib");
        umbrella.alias("/usr/lib/system/libsystem_c.dylib");
        assert!(umbrella.provides("/usr/lib/system/libsystem_c.dylib"));
        assert!(!umbrella.provides("/usr/lib/libobjc.A.dylib"));

        umbrella.flat(true);
        assert!(umbrella.provides("/usr/lib/libobjc.A.dylib"));
    }

    /// The umbrella's own definitions first, then the image it re-exports,
    /// and only then whatever comes after it in the chain.
    #[test]
    fn a_two_level_import_follows_the_stand_ins_reexport() {
        let mut umbrella = SyntheticDylib::new("/usr/lib/libSystem.B.dylib");
        umbrella
            .reexport("/usr/lib/libcopyfile.dylib")
            .define_fn("_write", 0x1111 as *const ());
        let mut copyfile = SyntheticDylib::new("/usr/lib/libcopyfile.dylib");
        copyfile.define_fn("_copyfile", 0x2222 as *const ());
        let mut fallback = SyntheticDylib::new("fallback");
        fallback
            .flat(true)
            .define_fn("_copyfile", 0x3333 as *const ());
        let providers: [&dyn SymbolProvider; 3] = [&umbrella, &copyfile, &fallback];

        let system = LibraryRef::InstallName("/usr/lib/libSystem.B.dylib");
        let found = |name| resolve_in(&providers, &sym(name, system)).map(|r| r.addr);
        assert_eq!(found("_write"), Some(0x1111));
        assert_eq!(found("_copyfile"), Some(0x2222));
        assert_eq!(found("_missing"), None);
        assert!(umbrella.reexports("/usr/lib/libobjc.A.dylib").is_empty());
    }

    #[test]
    fn earlier_providers_win() {
        let mut shim = SyntheticDylib::new("shims");
        shim.flat(true).define_fn("_environ", 0xaaaa as *const ());
        let mut real = SyntheticDylib::new("/usr/lib/libSystem.B.dylib");
        real.define_fn("_environ", 0xbbbb as *const ());

        let providers: [&dyn SymbolProvider; 2] = [&shim, &real];
        let r = resolve_in(
            &providers,
            &sym(
                "_environ",
                LibraryRef::InstallName("/usr/lib/libSystem.B.dylib"),
            ),
        );
        assert_eq!(
            r.map(|r| r.addr),
            Some(0xaaaa),
            "a flat shim must shadow the real library"
        );
    }
}
