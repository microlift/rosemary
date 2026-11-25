use alloc::string::ToString;
use alloc::vec::Vec;

use crate::error::Result;
use crate::exports::{ExportTable, ExportValue};
use crate::fixups::{BindKind, Fixup, LibraryRef, StubMap, SymbolRef};
use crate::memory::AddressSpace;
use crate::raw::Word;
use crate::report::{BindingRecord, MissingSymbol};
use crate::resolve::{Resolved, SymbolKind, SymbolProvider, resolve_in};

#[derive(Clone, Debug, Default)]
pub struct BindOutcome {
    pub bindings: Vec<BindingRecord>,
    pub missing: Vec<MissingSymbol>,
    /// Pointers slid.
    pub rebases: usize,
}

pub struct Binder<'s> {
    pub slide: Word,
    pub providers: &'s [&'s dyn SymbolProvider],
    pub self_exports: Option<&'s ExportTable>,
    pub main_executable: Option<(&'s ExportTable, Word)>,
}

impl<'s> Binder<'s> {
    pub fn new(slide: Word) -> Self {
        Binder {
            slide,
            providers: &[],
            self_exports: None,
            main_executable: None,
        }
    }

    pub fn apply<M: AddressSpace>(
        &self,
        stream: &[Fixup<'_>],
        mem: &mut M,
        stubs: &StubMap,
    ) -> Result<BindOutcome> {
        let mut out = BindOutcome::default();

        for fixup in stream {
            match fixup {
                Fixup::Rebase { at } => {
                    self.rebase(*at, mem)?;
                    out.rebases += 1;
                }
                Fixup::RebaseTo { at, target } => {
                    let value = target.wrapping_add(self.slide);
                    mem.write(at.wrapping_add(self.slide), &value.to_le_bytes())?;
                    out.rebases += 1;
                }
                Fixup::Bind {
                    at,
                    symbol,
                    addend,
                    kind,
                } => {
                    self.bind(*at, symbol, *addend, *kind, mem, stubs, &mut out)?;
                }
            }
        }
        Ok(out)
    }

    /// Add the slide to the pointer already stored at `at`.
    fn rebase<M: AddressSpace>(&self, at: Word, mem: &mut M) -> Result<()> {
        let addr = at.wrapping_add(self.slide);
        if self.slide == 0 {
            return Ok(());
        }
        let mut buf = [0u8; 8];
        mem.read(addr, &mut buf)?;
        let value = u64::from_le_bytes(buf).wrapping_add(self.slide);
        mem.write(addr, &value.to_le_bytes())
    }

    #[allow(clippy::too_many_arguments)]
    fn bind<M: AddressSpace>(
        &self,
        at: Word,
        symbol: &SymbolRef<'_>,
        addend: i64,
        kind: BindKind,
        mem: &mut M,
        stubs: &StubMap,
        out: &mut BindOutcome,
    ) -> Result<()> {
        let slot_va = at.wrapping_add(self.slide);
        let stub_va = stubs.stub_for(at).map(|s| s.wrapping_add(self.slide));
        let library = symbol.library.install_name().map(|n| n.to_string());

        let resolved = self.lookup(symbol);

        let Some(r) = resolved else {
            if symbol.library == LibraryRef::WeakLookup {
                return Ok(());
            }
            let value: u64 = 0;
            mem.write(slot_va, &value.to_le_bytes())?;
            out.missing.push(MissingSymbol {
                symbol: symbol.name.to_string(),
                library,
                slot_va,
                stub_va,
                weak_import: symbol.weak_import,
            });
            return Ok(());
        };

        let value = r.addr.wrapping_add(addend as u64);
        mem.write(slot_va, &value.to_le_bytes())?;

        out.bindings.push(BindingRecord {
            slot_va,
            stub_va,
            library,
            symbol: symbol.name.to_string(),
            resolved: value,
            addend,
            kind,
            weak_import: symbol.weak_import,
            tag: r.tag,
        });
        Ok(())
    }

    fn lookup(&self, symbol: &SymbolRef<'_>) -> Option<Resolved> {
        match symbol.library {
            LibraryRef::SelfImage => lookup_export(self.self_exports?, symbol.name, self.slide),
            LibraryRef::MainExecutable => {
                let (exports, slide) = self.main_executable?;
                lookup_export(exports, symbol.name, slide)
            }
            _ => resolve_in(self.providers, symbol),
        }
    }
}

fn lookup_export(exports: &ExportTable, name: &str, slide: Word) -> Option<Resolved> {
    let e = exports.get(name)?;
    if matches!(e.value, ExportValue::Reexport { .. }) {
        return None;
    }
    let addr = e.address(slide)?;
    Some(Resolved {
        addr,
        kind: match e.kind {
            crate::exports::ExportKind::ThreadLocal => SymbolKind::ThreadLocal,
            _ => SymbolKind::Function,
        },
        tag: 0,
    })
}
