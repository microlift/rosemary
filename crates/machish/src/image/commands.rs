use crate::error::Result;
use crate::image::{EntryPoint, Segment};
use crate::raw::View;
use crate::spec::constants as c;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DependencyKind {
    /// `LC_LOAD_DYLIB`: required.
    Load,
    /// `LC_LOAD_WEAK_DYLIB`: absence is not an error, and its symbols bind to
    /// zero rather than failing.
    Weak,
    /// `LC_REEXPORT_DYLIB`: this image republishes the dependency's exports as
    /// its own, so a two-level lookup against this image must search it.
    Reexport,
    /// `LC_LOAD_UPWARD_DYLIB`: a deliberate dependency cycle, used by system
    /// umbrellas.
    Upward,
}

impl DependencyKind {
    pub fn is_weak(self) -> bool {
        matches!(self, DependencyKind::Weak)
    }
}

/// A `LC_LOAD_DYLIB`-family entry.
#[derive(Copy, Clone, Debug)]
pub struct Dependency<'a> {
    /// The install name as recorded, which may be `@rpath`-relative and so is
    /// not necessarily a path that exists.
    pub install_name: &'a str,
    pub kind: DependencyKind,
    /// 1-based position among dependency commands. Bind opcodes reference
    /// dependencies by exactly this number, so it must count *all* dylib
    /// commands in file order, including weak and re-export ones.
    pub ordinal: u16,
    pub timestamp: u32,
    pub current_version: u32,
    pub compatibility_version: u32,
}

/// `LC_SYMTAB`: the classic symbol and string tables.
#[derive(Copy, Clone, Debug, Default)]
pub struct Symtab {
    pub symoff: u32,
    pub nsyms: u32,
    pub stroff: u32,
    pub strsize: u32,
}

/// `LC_DYSYMTAB`, reduced to the parts a loader needs: the indirect symbol
/// table, which is what maps a stub or pointer slot back to a symbol, and the
/// external/local relocation tables a pre-`LC_DYLD_INFO` image binds through.
#[derive(Copy, Clone, Debug, Default)]
pub struct Dysymtab {
    pub ilocalsym: u32,
    pub nlocalsym: u32,
    pub iextdefsym: u32,
    pub nextdefsym: u32,
    pub iundefsym: u32,
    pub nundefsym: u32,
    pub indirectsymoff: u32,
    pub nindirectsyms: u32,
    pub extreloff: u32,
    pub nextrel: u32,
    pub locreloff: u32,
    pub nlocrel: u32,
}

/// `LC_DYLD_INFO(_ONLY)`: the four fixup opcode streams plus the export trie.
#[derive(Copy, Clone, Debug, Default)]
pub struct DyldInfo {
    pub rebase_off: u32,
    pub rebase_size: u32,
    pub bind_off: u32,
    pub bind_size: u32,
    pub weak_bind_off: u32,
    pub weak_bind_size: u32,
    pub lazy_bind_off: u32,
    pub lazy_bind_size: u32,
    pub export_off: u32,
    pub export_size: u32,
}

/// A `(dataoff, datasize)` pair pointing into `__LINKEDIT`.
#[derive(Copy, Clone, Debug, Default)]
pub struct LinkeditData {
    pub dataoff: u32,
    pub datasize: u32,
}

impl LinkeditData {
    pub fn parse(view: &View<'_>, off: usize) -> Result<Self> {
        Ok(LinkeditData {
            dataoff: view.u32(off + 8)?,
            datasize: view.u32(off + 12)?,
        })
    }

    /// The byte range this refers to, or `None` if empty.
    pub fn range(&self) -> Option<core::ops::Range<usize>> {
        if self.datasize == 0 {
            return None;
        }
        let start = self.dataoff as usize;
        Some(start..start + self.datasize as usize)
    }
}

/// `LC_ENCRYPTION_INFO_64`.
#[derive(Copy, Clone, Debug, Default)]
pub struct EncryptionInfo {
    pub cryptoff: u32,
    pub cryptsize: u32,
    /// Nonzero means the `__TEXT` range is encrypted and only the kernel's
    /// pager can decrypt it.
    pub cryptid: u32,
}

/// One decoded load command.
#[derive(Clone, Debug)]
pub enum LoadCommand<'a> {
    Segment(Segment<'a>),
    Symtab(Symtab),
    Dysymtab(Dysymtab),
    Dylib(Dependency<'a>),
    /// `LC_ID_DYLIB`: this image's own install name, present only in a dylib.
    IdDylib(&'a str),
    /// `LC_RPATH`: one entry of the `@rpath` search stack.
    Rpath(&'a str),
    /// `LC_LOAD_DYLINKER`, e.g. `/usr/lib/dyld`.
    Dylinker(&'a str),
    Entry(EntryPoint),
    DyldInfo(DyldInfo),
    /// `LC_DYLD_CHAINED_FIXUPS`.
    ChainedFixups(LinkeditData),
    /// `LC_DYLD_EXPORTS_TRIE`.
    ExportsTrie(LinkeditData),
    FunctionStarts(LinkeditData),
    CodeSignature(LinkeditData),
    EncryptionInfo(EncryptionInfo),
    Uuid([u8; 16]),
    /// Anything the loader does not interpret, kept for inspection.
    Other {
        cmd: u32,
        cmdsize: u32,
        /// Offset of the command within the image.
        offset: usize,
    },
}

impl<'a> LoadCommand<'a> {
    /// Parse the command at `off`. Segments are returned with an empty section
    /// range; [`MachObject`](crate::image::MachObject) fills it in, since it
    /// owns the flat section table.
    ///
    /// `dylib_ordinal` is the running count of dylib commands seen so far, used
    /// to assign [`Dependency::ordinal`].
    pub fn parse(view: &View<'a>, off: usize, dylib_ordinal: u16) -> Result<(Self, u32)> {
        let cmd = view.u32(off)?;
        let cmdsize = view.u32(off + 4)?;

        // A load command's variable-length string operand is stored as an
        // offset from the start of the command, not from the string field.
        let lc_str = |field: usize| -> Result<&'a str> {
            let str_off = view.u32(off + field)? as usize;
            view.cstr(
                off.checked_add(str_off)
                    .ok_or(crate::Error::BadLoadCommand {
                        why: "string offset overflow",
                    })?,
            )
        };

        let parsed = match cmd {
            c::LC_SEGMENT_64 => LoadCommand::Segment(Segment::parse(view, off)?.0),

            c::LC_SYMTAB => LoadCommand::Symtab(Symtab {
                symoff: view.u32(off + 8)?,
                nsyms: view.u32(off + 12)?,
                stroff: view.u32(off + 16)?,
                strsize: view.u32(off + 20)?,
            }),

            c::LC_DYSYMTAB => LoadCommand::Dysymtab(Dysymtab {
                ilocalsym: view.u32(off + 8)?,
                nlocalsym: view.u32(off + 12)?,
                iextdefsym: view.u32(off + 16)?,
                nextdefsym: view.u32(off + 20)?,
                iundefsym: view.u32(off + 24)?,
                nundefsym: view.u32(off + 28)?,
                indirectsymoff: view.u32(off + 56)?,
                nindirectsyms: view.u32(off + 60)?,
                extreloff: view.u32(off + 64)?,
                nextrel: view.u32(off + 68)?,
                locreloff: view.u32(off + 72)?,
                nlocrel: view.u32(off + 76)?,
            }),

            c::LC_LOAD_DYLIB
            | c::LC_LOAD_WEAK_DYLIB
            | c::LC_REEXPORT_DYLIB
            | c::LC_LOAD_UPWARD_DYLIB => LoadCommand::Dylib(Dependency {
                install_name: lc_str(8)?,
                kind: match cmd {
                    c::LC_LOAD_WEAK_DYLIB => DependencyKind::Weak,
                    c::LC_REEXPORT_DYLIB => DependencyKind::Reexport,
                    c::LC_LOAD_UPWARD_DYLIB => DependencyKind::Upward,
                    _ => DependencyKind::Load,
                },
                ordinal: dylib_ordinal,
                timestamp: view.u32(off + 12)?,
                current_version: view.u32(off + 16)?,
                compatibility_version: view.u32(off + 20)?,
            }),

            c::LC_ID_DYLIB => LoadCommand::IdDylib(lc_str(8)?),
            c::LC_RPATH => LoadCommand::Rpath(lc_str(8)?),
            c::LC_LOAD_DYLINKER => LoadCommand::Dylinker(lc_str(8)?),

            c::LC_MAIN => LoadCommand::Entry(EntryPoint::parse_main(view, off)?),
            c::LC_UNIXTHREAD => {
                LoadCommand::Entry(EntryPoint::parse_unix_thread(view, off, cmdsize as usize)?)
            }

            c::LC_DYLD_INFO | c::LC_DYLD_INFO_ONLY => LoadCommand::DyldInfo(DyldInfo {
                rebase_off: view.u32(off + 8)?,
                rebase_size: view.u32(off + 12)?,
                bind_off: view.u32(off + 16)?,
                bind_size: view.u32(off + 20)?,
                weak_bind_off: view.u32(off + 24)?,
                weak_bind_size: view.u32(off + 28)?,
                lazy_bind_off: view.u32(off + 32)?,
                lazy_bind_size: view.u32(off + 36)?,
                export_off: view.u32(off + 40)?,
                export_size: view.u32(off + 44)?,
            }),

            c::LC_DYLD_CHAINED_FIXUPS => {
                LoadCommand::ChainedFixups(LinkeditData::parse(view, off)?)
            }
            c::LC_DYLD_EXPORTS_TRIE => LoadCommand::ExportsTrie(LinkeditData::parse(view, off)?),
            c::LC_FUNCTION_STARTS => LoadCommand::FunctionStarts(LinkeditData::parse(view, off)?),
            c::LC_CODE_SIGNATURE => LoadCommand::CodeSignature(LinkeditData::parse(view, off)?),

            c::LC_ENCRYPTION_INFO_64 => LoadCommand::EncryptionInfo(EncryptionInfo {
                cryptoff: view.u32(off + 8)?,
                cryptsize: view.u32(off + 12)?,
                cryptid: view.u32(off + 16)?,
            }),

            c::LC_UUID => {
                let b = view.slice(off + 8, 16, "uuid")?;
                let mut uuid = [0u8; 16];
                uuid.copy_from_slice(b);
                LoadCommand::Uuid(uuid)
            }

            _ => LoadCommand::Other {
                cmd,
                cmdsize,
                offset: off,
            },
        };

        Ok((parsed, cmdsize))
    }

    /// Whether this command contributes a dependency ordinal.
    pub fn is_dylib(&self) -> bool {
        matches!(self, LoadCommand::Dylib(_))
    }
}

/// Whether `cmd` is one of the dylib-loading commands, and so advances the
/// ordinal counter. Checked before parsing, since the ordinal is an input.
pub(crate) fn is_dylib_command(cmd: u32) -> bool {
    matches!(
        cmd,
        c::LC_LOAD_DYLIB | c::LC_LOAD_WEAK_DYLIB | c::LC_REEXPORT_DYLIB | c::LC_LOAD_UPWARD_DYLIB
    )
}
