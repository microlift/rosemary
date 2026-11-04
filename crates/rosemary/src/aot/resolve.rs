//! What a cached image's symbols and import decisions mean in this process.
//!
//! Symbols resolve through the same providers a live lift consults: the program's
//! import sites as [`Program::lifter_config`] promotes them, this runtime's bridges,
//! and the host's own libcalls. Nothing is matched by name alone.

use std::collections::HashMap;

use microlift::core::LifterConfig;
use microlift::core::bridge::ImportKind;
use microlift::core::symbol::{HostSymbol, HostSymbolKind, ImportDecision};
use microlift::runtime::image::SymbolResolver;

/// Resolves an image's symbols against one program's config.
pub struct Resolver {
    symbols: HashMap<HostSymbol, usize>,
    slide: u64,
}

impl Resolver {
    /// `config` is the program's lifter config; `slide` is the image's run-time minus
    /// lift-time address.
    pub fn new(config: &LifterConfig, slide: u64) -> Self {
        let mut symbols = HashMap::new();
        for kind in config.imports.values() {
            if let ImportKind::Fast(import) = kind {
                symbols.insert(import.host_symbol(), import.symbol.value as usize);
            }
        }
        for bridge in [
            crate::bridge::import_bridge(),
            crate::bridge::indirect_bridge(),
        ] {
            if let microlift::core::bridge::BridgeTarget::Direct(address) = bridge.target {
                symbols.insert(bridge.symbol(), address as usize);
            }
        }
        Resolver { symbols, slide }
    }
}

impl SymbolResolver for Resolver {
    fn resolve(&self, symbol: &HostSymbol) -> Option<usize> {
        if let Some(&address) = self.symbols.get(symbol) {
            return Some(address);
        }
        (symbol.kind == HostSymbolKind::LibCall).then(|| libcall(&symbol.name))?
    }

    fn guest_slide(&self) -> u64 {
        self.slide
    }
}

/// A Cranelift runtime routine, from the host's C library as the JIT finds it.
fn libcall(name: &str) -> Option<usize> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: a lookup in the default namespace with a NUL-terminated name.
    let address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!address.is_null()).then_some(address as usize)
}

/// What `config` answers for `va` now, consulting the registry as a lift would.
fn decision_now(config: &LifterConfig, va: u64) -> ImportDecision {
    let kind = match config.imports.get(&va) {
        Some(kind) => Some(kind.clone()),
        None => config.import_registry.as_ref().and_then(|r| r.lookup(va)),
    };
    ImportDecision::of(kind.as_ref())
}

/// Whether a region lifted with these decisions may be used under `config`: every
/// lookup it made must get the same answer today. `dlopen` order, an interposer or a
/// traced symbol can each change one.
pub fn decisions_hold(config: &LifterConfig, decisions: &[(u64, ImportDecision)]) -> bool {
    decisions
        .iter()
        .all(|(va, decision)| decision_now(config, *va) == *decision)
}

/// Whether a region may go into a cache at all: only sites the program's own
/// bindings answer. A site only `dlsym` or the import registry can answer depends on
/// what this run happened to load.
pub fn cacheable(config: &LifterConfig, decisions: &[(u64, ImportDecision)]) -> bool {
    decisions.iter().all(|(va, decision)| {
        *decision == ImportDecision::NotImport || config.imports.contains_key(va)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use microlift::core::abi::CallAbi;
    use microlift::core::symbol::RUNTIME_PROVIDER;
    use microlift::function_traits::{DynamicSymbol, FunctionArgumentType};

    unsafe extern "C" fn from_a() {}
    unsafe extern "C" fn from_b() {}

    fn symbol(name: &'static str, f: unsafe extern "C" fn()) -> DynamicSymbol {
        DynamicSymbol {
            name,
            value: f as *mut _,
            arg_types: vec![FunctionArgumentType::None],
            is_ellipsis_function: false,
            has_execution_side_effects: false,
        }
    }

    fn config() -> LifterConfig {
        let mut config = LifterConfig::default();
        config.register_fast_import_from(
            0x10,
            "libA.dylib",
            symbol("_f", from_a),
            CallAbi::sysv_amd64(),
        );
        config.register_fast_import_from(
            0x20,
            "libB.dylib",
            symbol("_f", from_b),
            CallAbi::sysv_amd64(),
        );
        config.register_slow_import(0x30);
        config
    }

    #[test]
    fn a_symbol_resolves_by_provider_not_by_name() {
        let config = config();
        let resolver = Resolver::new(&config, 0);
        let ImportKind::Fast(b) = &config.imports[&0x20] else {
            unreachable!()
        };
        assert_eq!(
            resolver.resolve(&b.host_symbol()),
            Some(from_b as *const () as usize)
        );
        let mut elsewhere = b.host_symbol();
        elsewhere.provider = "libC.dylib".into();
        assert_eq!(resolver.resolve(&elsewhere), None);
    }

    #[test]
    fn the_runtime_bridges_resolve() {
        let resolver = Resolver::new(&LifterConfig::default(), 0);
        let bridge = crate::bridge::import_bridge();
        assert_eq!(bridge.symbol().provider.as_ref(), RUNTIME_PROVIDER);
        assert!(resolver.resolve(&bridge.symbol()).is_some());
    }

    #[test]
    fn a_changed_import_answer_invalidates_a_region() {
        let config = config();
        let recorded = vec![
            (0x30, ImportDecision::Slow),
            (0x40, ImportDecision::NotImport),
        ];
        assert!(decisions_hold(&config, &recorded));
        let mut changed = config.clone();
        changed.register_slow_import(0x40);
        assert!(!decisions_hold(&changed, &recorded));
    }

    #[test]
    fn a_site_the_program_does_not_bind_is_not_cacheable() {
        let config = config();
        assert!(cacheable(&config, &[(0x30, ImportDecision::Slow)]));
        assert!(!cacheable(
            &config,
            &[(
                0x50,
                ImportDecision::Bridge {
                    name: "rosemary-import".into()
                }
            )]
        ));
    }
}
