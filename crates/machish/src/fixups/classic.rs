use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::fixups::{BindKind, Fixup, FixupDecoder, LibraryRef, SymbolRef};
use crate::image::{Dysymtab, MachObject, Nlist, SymbolTable};
use crate::raw::Word;
use crate::spec::MachSpec;
use crate::spec::constants as c;

/// `n_desc` library ordinal meaning "search every image", equivalent
/// to `BIND_SPECIAL_DYLIB_FLAT_LOOKUP`.
const DYNAMIC_LOOKUP_ORDINAL: u16 = 0xfe;
/// `n_desc` library ordinal naming the main executable.
const EXECUTABLE_ORDINAL: u16 = 0xff;

#[derive(Copy, Clone, Debug, Default)]
pub struct ClassicFixups;

impl<S: MachSpec> FixupDecoder<S> for ClassicFixups {
    fn name(&self) -> &'static str {
        "classic"
    }

    fn claims(&self, obj: &MachObject<'_, S>) -> bool {
        obj.dyld_info().is_none()
            && obj.chained_fixups().is_none()
            && obj.symtab().is_some()
            && obj
                .dysymtab()
                .is_some_and(|d| d.nindirectsyms != 0 || d.nextrel != 0 || d.nlocrel != 0)
    }

    fn decode<'a>(&self, obj: &MachObject<'a, S>, out: &mut Vec<Fixup<'a>>) -> Result<()> {
        let (Some(symtab), Some(dysymtab)) = (obj.symtab(), obj.dysymtab()) else {
            return Ok(());
        };
        let table = SymbolTable::new(*obj.view(), symtab, Some(dysymtab))?;

        for sect in &obj.sections {
            if !sect.is_symbol_pointers() {
                continue;
            }
            let lazy = sect.is_lazy_pointers();
            for j in 0..sect.entry_count(c::POINTER_SIZE) {
                let index = usize::try_from(u64::from(sect.reserved1) + j).map_err(|_| {
                    Error::Truncated {
                        what: "indirect symbol index",
                    }
                })?;
                let at = sect.addr.wrapping_add(j * c::POINTER_SIZE);
                // A `LOCAL` sentinel means the slot holds an address in this
                // image. dyld slides those in non-lazy sections itself; a lazy
                // slot's stub-helper address is a local relocation instead.
                let raw = table.indirect(index)?;
                if raw & c::INDIRECT_SYMBOL_LOCAL != 0 {
                    if !lazy && raw & c::INDIRECT_SYMBOL_ABS == 0 {
                        out.push(Fixup::Rebase { at });
                    }
                    continue;
                }
                let Some(sym) = table.indirect_symbol(index)? else {
                    continue;
                };
                if !sym.is_undefined() {
                    continue;
                }
                let name = table.name(&sym)?;
                out.push(Fixup::Bind {
                    at,
                    symbol: SymbolRef {
                        name,
                        library: library_ref(obj, &sym),
                        weak_import: sym.is_weak_reference(),
                    },
                    addend: 0,
                    kind: if lazy {
                        BindKind::Lazy
                    } else {
                        BindKind::Indirect
                    },
                });
            }
        }

        decode_relocations(obj, &table, &dysymtab, out)
    }
}

/// One `relocation_info`, unpacked.
struct Relocation {
    address: u32,
    symbolnum: u32,
    pcrel: bool,
    length: u8,
    is_extern: bool,
    kind: u8,
}

impl Relocation {
    fn read<S: MachSpec>(obj: &MachObject<'_, S>, off: usize) -> Result<Self> {
        let address = obj.view().u32(off)?;
        let info = obj.view().u32(off + 4)?;
        Ok(Relocation {
            address,
            symbolnum: info & 0x00ff_ffff,
            pcrel: info & (1 << 24) != 0,
            length: ((info >> 25) & 3) as u8,
            is_extern: info & (1 << 27) != 0,
            kind: (info >> 28) as u8,
        })
    }

    /// Only an absolute pointer-sized store is something dyld applies to a
    /// linked image; anything else means a malformed or unsupported image.
    fn check_pointer(&self) -> Result<()> {
        if self.address & c::R_SCATTERED != 0 {
            return Err(Error::Custom("scattered relocation in a 64-bit image"));
        }
        if self.pcrel || self.length != c::RELOC_LENGTH_POINTER || self.kind != c::RELOC_UNSIGNED {
            return Err(Error::Custom("unsupported classic relocation type"));
        }
        Ok(())
    }
}

/// The external and local relocation tables of a pre-`LC_DYLD_INFO` image.
///
/// Toolchains before 10.6 bind data pointers - Objective-C `message_ref_t`
/// IMPs, class and superclass references, CFString isa words - only through
/// external relocations, so ignoring these leaves those slots zero.
fn decode_relocations<'a, S: MachSpec>(
    obj: &MachObject<'a, S>,
    table: &SymbolTable<'a>,
    dysymtab: &Dysymtab,
    out: &mut Vec<Fixup<'a>>,
) -> Result<()> {
    if dysymtab.nextrel == 0 && dysymtab.nlocrel == 0 {
        return Ok(());
    }
    let base = reloc_base(obj)?;

    for i in 0..dysymtab.nextrel as usize {
        let r = Relocation::read(
            obj,
            dysymtab.extreloff as usize + i * c::RELOCATION_INFO_SIZE,
        )?;
        if !r.is_extern {
            return Err(Error::Custom(
                "non-extern entry in the external relocations",
            ));
        }
        r.check_pointer()?;
        let at = base.wrapping_add(Word::from(r.address));
        let sym = table.nlist(r.symbolnum as usize)?;
        out.push(Fixup::Bind {
            at,
            symbol: SymbolRef {
                name: table.name(&sym)?,
                library: if sym.is_undefined() {
                    library_ref(obj, &sym)
                } else {
                    LibraryRef::SelfImage
                },
                weak_import: sym.is_weak_reference(),
            },
            // dyld adds the target to what the slot already holds.
            addend: stored_word(obj, at)? as i64,
            kind: BindKind::NonLazy,
        });
    }

    for i in 0..dysymtab.nlocrel as usize {
        let r = Relocation::read(
            obj,
            dysymtab.locreloff as usize + i * c::RELOCATION_INFO_SIZE,
        )?;
        if r.is_extern {
            return Err(Error::Custom("extern entry in the local relocations"));
        }
        r.check_pointer()?;
        out.push(Fixup::Rebase {
            at: base.wrapping_add(Word::from(r.address)),
        });
    }
    Ok(())
}

/// What `r_address` is relative to: on x86_64 and arm64 dyld uses the first
/// writable segment rather than the first segment.
fn reloc_base<S: MachSpec>(obj: &MachObject<'_, S>) -> Result<Word> {
    obj.segments
        .iter()
        .find(|s| s.initprot & c::VM_PROT_WRITE != 0)
        .map(|s| s.vmaddr)
        .ok_or(Error::Custom(
            "relocations in an image with no writable segment",
        ))
}

/// The pointer the file stores at `at`, or zero for a zero-fill slot.
fn stored_word<S: MachSpec>(obj: &MachObject<'_, S>, at: Word) -> Result<Word> {
    match obj.vmaddr_to_off(at) {
        Some(off) => obj.view().u64(off),
        None => Ok(0),
    }
}

fn library_ref<'a, S: MachSpec>(obj: &MachObject<'a, S>, sym: &Nlist) -> LibraryRef<'a> {
    if !obj.is_two_level() {
        return LibraryRef::Flat;
    }
    match sym.library_ordinal() {
        0 => LibraryRef::SelfImage,
        DYNAMIC_LOOKUP_ORDINAL => LibraryRef::Flat,
        EXECUTABLE_ORDINAL => LibraryRef::MainExecutable,
        n => match obj.dependency_by_ordinal(n) {
            Some(dep) => LibraryRef::InstallName(dep.install_name),
            None => LibraryRef::UnknownOrdinal(n),
        },
    }
}
