//! Names for addresses in an image, for showing what is being compiled.

use machish::image::{MachObject, SymbolTable};
use machish::report::ImageReport;
use machish::spec::arch::X86_64;
use machish::spec::constants::N_SECT;

/// An image's code symbols and Objective-C methods, by slid address.
#[derive(Debug, Default)]
pub struct Names {
    /// Ascending, each address once.
    symbols: Vec<(u64, Box<str>)>,
    /// Slid function starts, ascending: a symbol covers an address only up to the
    /// next of these.
    starts: Vec<u64>,
}

impl Names {
    /// Names in `bytes`, the file of `report`, with `function_starts` unslid.
    pub fn of(bytes: &[u8], report: &ImageReport, function_starts: &[u64]) -> Self {
        let slide = report.slide;
        let in_code = |va: u64| {
            report
                .segments
                .iter()
                .any(|s| s.executable && va.wrapping_sub(s.vmaddr) < s.vmsize)
        };
        let mut symbols = Vec::new();
        if let Ok(obj) = MachObject::parse_any(bytes, X86_64)
            && let Some(symtab) = obj.symtab()
            && let Ok(table) = SymbolTable::new(*obj.view(), symtab, obj.dysymtab())
        {
            for (_, sym) in table.iter() {
                if sym.is_stab() || sym.symbol_type() != N_SECT {
                    continue;
                }
                let va = sym.n_value.wrapping_add(slide);
                if let Ok(name) = table.name(&sym)
                    && !name.is_empty()
                    && in_code(va)
                {
                    // The C-level name: the linker prefixes every symbol with `_`.
                    symbols.push((va, name.strip_prefix('_').unwrap_or(name).into()));
                }
            }
        }
        // After the symbol table, so a stripped image still names its methods but
        // an unstripped one keeps its own spelling.
        if let Some(objc) = &report.objc {
            for class in &objc.classes {
                let methods = class.ro.methods.iter().map(|m| ('-', m));
                let class_methods = class.meta.iter().flat_map(|meta| meta.ro.methods.iter());
                for (kind, method) in methods.chain(class_methods.map(|m| ('+', m))) {
                    let name = format!("{kind}[{} {}]", class.ro.name, method.name);
                    symbols.push((method.imp, name.into()));
                }
            }
            for category in &objc.categories {
                let instance = category.instance_methods.iter().map(|m| ('-', m));
                let class = category.class_methods.iter().map(|m| ('+', m));
                for (kind, method) in instance.chain(class) {
                    let name = format!("{kind}[({}) {}]", category.name, method.name);
                    symbols.push((method.imp, name.into()));
                }
            }
        }
        symbols.sort_by_key(|&(va, _)| va);
        symbols.dedup_by_key(|&mut (va, _)| va);
        Names {
            symbols,
            starts: function_starts
                .iter()
                .map(|start| start.wrapping_add(slide))
                .collect(),
        }
    }

    /// The symbol `address` is in and how far into it, if the symbol covers it.
    pub fn name(&self, address: u64) -> Option<(&str, u64)> {
        let at = self.symbols.partition_point(|&(va, _)| va <= address);
        let (start, name) = self.symbols.get(at.checked_sub(1)?)?;
        let function = self.starts.partition_point(|&va| va <= address);
        let function = function.checked_sub(1).map(|n| self.starts[n]);
        if function.is_some_and(|function| function > *start) {
            return None;
        }
        Some((name, address - start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Names {
        Names {
            symbols: vec![(0x100, "main".into()), (0x200, "helper".into())],
            starts: vec![0x100, 0x180, 0x200],
        }
    }

    #[test]
    fn names_an_address_inside_its_function() {
        assert_eq!(names().name(0x100), Some(("main", 0)));
        assert_eq!(names().name(0x210), Some(("helper", 0x10)));
    }

    #[test]
    fn a_symbol_does_not_cover_a_later_unnamed_function() {
        assert_eq!(names().name(0x190), None);
        assert_eq!(names().name(0x80), None);
    }
}
