use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::error::{Error, Result};
use crate::image::commands::{
    Dependency, DyldInfo, Dysymtab, LinkeditData, LoadCommand, Symtab, is_dylib_command,
};
use crate::image::{EntryPoint, Section, Segment};
use crate::raw::{View, Word};
use crate::spec::MachSpec;
use crate::spec::constants::{self as c, LOAD_COMMAND_SIZE, MACH_HEADER_64_SIZE, SECTION_64_SIZE};
use crate::{fat, memory::align_up};

/// A guard against a malformed `ncmds`. Real images carry a few dozen.
const MAX_LOAD_COMMANDS: u32 = 4096;

/// A parsed Mach-O image borrowing the file bytes.
///
/// The lifetime is the *slice's*: for a fat file, [`parse_any`](Self::parse_any)
/// narrows to the matching architecture first, and every offset stored here is
/// relative to that slice rather than to the containing archive.
pub struct MachObject<'a, S: MachSpec> {
    bytes: &'a [u8],
    view: View<'a>,
    spec: S,

    pub cputype: i32,
    pub cpusubtype: i32,
    pub filetype: u32,
    pub flags: u32,

    /// Every load command, in file order.
    pub commands: Vec<LoadCommand<'a>>,
    /// Every `LC_SEGMENT_64`, in file order.
    pub segments: Vec<Segment<'a>>,
    /// Every section, flattened; [`Segment::sections`] indexes into this.
    pub sections: Vec<Section<'a>>,
    /// Dependencies in ordinal order, so index `n` is ordinal `n + 1`.
    dependencies: Vec<Dependency<'a>>,

    _spec: PhantomData<S>,
}

impl<'a, S: MachSpec> MachObject<'a, S> {
    /// Parse a thin Mach-O image, validating it against `spec`.
    pub fn parse(bytes: &'a [u8], spec: S) -> Result<Self> {
        let view = View::new(bytes);

        let magic = view.u32(0)?;
        if magic != c::MH_MAGIC_64 {
            // Distinguish "a Mach-O we refuse" from "not a Mach-O": the first
            // is a scope decision, the second means the caller passed the wrong
            // file, and they want different things done about them.
            return Err(if fat::is_rejected_macho_magic(magic) {
                Error::UnsupportedMachOForm(magic)
            } else {
                Error::BadMagic(magic)
            });
        }

        let cputype = view.i32(4)?;
        if cputype != S::CPU_TYPE {
            return Err(Error::WrongCpuType {
                expected: S::CPU_TYPE,
                found: cputype,
            });
        }
        let cpusubtype = view.i32(8)?;
        let filetype = view.u32(12)?;
        if !matches!(filetype, c::MH_EXECUTE | c::MH_DYLIB | c::MH_BUNDLE) {
            return Err(Error::UnsupportedFiletype(filetype));
        }

        let ncmds = view.u32(16)?;
        let sizeofcmds = view.u32(20)? as usize;
        let flags = view.u32(24)?;

        if ncmds > MAX_LOAD_COMMANDS {
            return Err(Error::TooDeep("load command count"));
        }
        // Bound the walk by the declared extent and the file, so a command
        // chain can never wander past either.
        let cmds_end =
            MACH_HEADER_64_SIZE
                .checked_add(sizeofcmds)
                .ok_or(Error::BadLoadCommand {
                    why: "sizeofcmds overflow",
                })?;
        if cmds_end > bytes.len() {
            return Err(Error::Truncated {
                what: "load commands",
            });
        }

        let mut obj = MachObject {
            bytes,
            view,
            spec,
            cputype,
            cpusubtype,
            filetype,
            flags,
            commands: Vec::with_capacity(ncmds as usize),
            segments: Vec::new(),
            sections: Vec::new(),
            dependencies: Vec::new(),
            _spec: PhantomData,
        };
        obj.walk_commands(ncmds, cmds_end)?;
        Ok(obj)
    }

    /// Parse `bytes`, transparently selecting the slice matching `spec` if it
    /// is a fat/universal file.
    pub fn parse_any(bytes: &'a [u8], spec: S) -> Result<Self> {
        Self::parse(fat::select(bytes, &spec)?, spec)
    }

    fn walk_commands(&mut self, ncmds: u32, cmds_end: usize) -> Result<()> {
        let mut off = MACH_HEADER_64_SIZE;
        let mut dylib_ordinal: u16 = 0;

        for _ in 0..ncmds {
            if off + LOAD_COMMAND_SIZE > cmds_end {
                return Err(Error::BadLoadCommand {
                    why: "command header past sizeofcmds",
                });
            }
            let cmd = self.view.u32(off)?;
            let cmdsize = self.view.u32(off + 4)? as usize;

            // Three separate hazards, each of which has bitten real loaders:
            // a zero size loops forever, a misaligned size desynchronizes every
            // subsequent field, and an oversized one reads another command's
            // memory as this one's payload.
            if cmdsize == 0 {
                return Err(Error::BadLoadCommand {
                    why: "zero cmdsize",
                });
            }
            if !cmdsize.is_multiple_of(8) {
                return Err(Error::BadLoadCommand {
                    why: "unaligned cmdsize",
                });
            }
            if off + cmdsize > cmds_end {
                return Err(Error::BadLoadCommand {
                    why: "cmdsize past sizeofcmds",
                });
            }
            // A command must be large enough to hold its own fixed fields.
            // Without this, an undersized segment command is caught only when a
            // field read happens to run off the end of the file, which
            // reports a confusing `Truncated` and would not fire at all for a
            // command sitting in the middle of a large image.
            if cmd == c::LC_SEGMENT_64 && cmdsize < c::SEGMENT_COMMAND_64_SIZE {
                return Err(Error::BadLoadCommand {
                    why: "segment command too small",
                });
            }

            if is_dylib_command(cmd) {
                dylib_ordinal += 1;
            }
            let (parsed, _) = LoadCommand::parse(&self.view, off, dylib_ordinal)?;

            match parsed {
                LoadCommand::Segment(mut seg) => {
                    let (_, nsects) = Segment::parse(&self.view, off)?;
                    let sects_off = Segment::sections_offset(off);
                    let need = (nsects as usize).checked_mul(SECTION_64_SIZE).ok_or(
                        Error::BadLoadCommand {
                            why: "nsects overflow",
                        },
                    )?;
                    if sects_off + need > off + cmdsize {
                        return Err(Error::BadLoadCommand {
                            why: "sections past segment command",
                        });
                    }

                    let start = self.sections.len();
                    for i in 0..nsects as usize {
                        self.sections
                            .push(Section::parse(&self.view, sects_off + i * SECTION_64_SIZE)?);
                    }
                    seg.sections = start..self.sections.len();
                    self.segments.push(seg.clone());
                    self.commands.push(LoadCommand::Segment(seg));
                }
                LoadCommand::Dylib(dep) => {
                    self.dependencies.push(dep);
                    self.commands.push(LoadCommand::Dylib(dep));
                }
                other => self.commands.push(other),
            }

            off += cmdsize;
        }
        Ok(())
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn view(&self) -> &View<'a> {
        &self.view
    }

    pub fn spec(&self) -> &S {
        &self.spec
    }

    /// Sections belonging to `seg`.
    pub fn sections_of(&self, seg: &Segment<'a>) -> &[Section<'a>] {
        &self.sections[seg.sections.clone()]
    }

    /// Find a section by segment and section name.
    pub fn section(&self, seg_name: &str, sect_name: &str) -> Option<&Section<'a>> {
        self.sections
            .iter()
            .find(|s| s.seg_name == seg_name && s.sect_name == sect_name)
    }

    /// The section containing `vmaddr`, if any.
    pub fn section_at(&self, vmaddr: Word) -> Option<&Section<'a>> {
        self.sections.iter().find(|s| s.contains(vmaddr))
    }

    /// Segments the loader will actually map, i.e. everything but `__PAGEZERO`.
    pub fn mappable_segments(&self) -> impl Iterator<Item = &Segment<'a>> {
        self.segments.iter().filter(|s| s.is_mappable())
    }

    /// The image's preferred load address: where its Mach header lands, the
    /// `vmaddr` of the segment mapping file offset zero. For a typical x86-64
    /// executable this is `0x1_0000_0000`, the address just past `__PAGEZERO`.
    ///
    /// Not simply the lowest segment: dyld measures export, function-start and
    /// unwind offsets from the header, and an image may place a zero-fill
    /// segment below it (Wine's loader reserves `[0x1000, 0x2_0000_0000)`).
    pub fn preferred_base(&self) -> Result<Word> {
        self.mappable_segments()
            .find(|s| s.fileoff == 0 && s.filesize > 0)
            .or_else(|| self.mappable_segments().min_by_key(|s| s.vmaddr))
            .map(|s| s.vmaddr)
            .ok_or(Error::NoLoadableSegments)
    }

    /// Total virtual span to reserve, from the preferred base to the end of the
    /// last mappable segment, rounded up to the image's page size.
    pub fn vm_span(&self) -> Result<Word> {
        let page = self.spec.page_size();
        let min = self.preferred_base()?;
        let max = self
            .mappable_segments()
            .map(|s| align_up(s.vmend(), page))
            .max()
            .ok_or(Error::NoLoadableSegments)?;
        max.checked_sub(min).ok_or(Error::Memory("negative span"))
    }

    /// Whether the image tolerates being loaded at an address other than its
    /// preferred one. Dylibs and bundles always do; executables must say so.
    pub fn is_pie(&self) -> bool {
        self.flags & c::MH_PIE != 0 || matches!(self.filetype, c::MH_DYLIB | c::MH_BUNDLE)
    }

    /// Whether imports are scoped to a specific library rather than searched
    /// flatly across every loaded image.
    pub fn is_two_level(&self) -> bool {
        self.flags & c::MH_TWOLEVEL != 0 && self.flags & c::MH_FORCE_FLAT == 0
    }

    pub fn entry(&self) -> Option<EntryPoint> {
        self.commands.iter().find_map(|cmd| match cmd {
            LoadCommand::Entry(e) => Some(*e),
            _ => None,
        })
    }

    /// Dependencies in ordinal order.
    pub fn dependencies(&self) -> &[Dependency<'a>] {
        &self.dependencies
    }

    /// Look up a dependency by its 1-based bind ordinal.
    pub fn dependency_by_ordinal(&self, ordinal: u16) -> Option<&Dependency<'a>> {
        if ordinal == 0 {
            return None;
        }
        self.dependencies.get(usize::from(ordinal) - 1)
    }

    /// This image's own install name, from `LC_ID_DYLIB`.
    pub fn install_name(&self) -> Option<&'a str> {
        self.commands.iter().find_map(|cmd| match cmd {
            LoadCommand::IdDylib(n) => Some(*n),
            _ => None,
        })
    }

    /// `LC_RPATH` entries, in file order - the order they are searched.
    pub fn rpaths(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.commands.iter().filter_map(|cmd| match cmd {
            LoadCommand::Rpath(p) => Some(*p),
            _ => None,
        })
    }

    /// Whether dyld would ignore `DYLD_*` environment variables for this image.
    pub fn restricts_dyld_environment(&self) -> bool {
        const CS_RUNTIME: u32 = 0x1_0000;
        const SUPER_BLOB: u32 = 0xfade_0cc0;
        const CODE_DIRECTORY: u32 = 0xfade_0c02;
        const ALLOW_ENV: &[u8] = b"com.apple.security.cs.allow-dyld-environment-variables";

        if self.section("__RESTRICT", "__restrict").is_some() {
            return true;
        }
        let Some(sig) = self.commands.iter().find_map(|cmd| match cmd {
            LoadCommand::CodeSignature(d) => Some(*d),
            _ => None,
        }) else {
            return false;
        };
        let view = &self.view;
        let Ok(blob) = view.slice(
            sig.dataoff as usize,
            sig.datasize as usize,
            "code signature",
        ) else {
            return false;
        };
        let be = |off: usize| {
            blob.get(off..off + 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        if be(0) != Some(SUPER_BLOB) {
            return false;
        }
        let hardened = (0..be(8).unwrap_or(0).min(64) as usize).any(|i| {
            let Some(at) = be(16 + i * 8).map(|o| o as usize) else {
                return false;
            };
            be(at) == Some(CODE_DIRECTORY) && be(at + 12).is_some_and(|f| f & CS_RUNTIME != 0)
        });
        hardened && !blob.windows(ALLOW_ENV.len()).any(|w| w == ALLOW_ENV)
    }

    pub fn symtab(&self) -> Option<Symtab> {
        self.commands.iter().find_map(|cmd| match cmd {
            LoadCommand::Symtab(s) => Some(*s),
            _ => None,
        })
    }

    pub fn dysymtab(&self) -> Option<Dysymtab> {
        self.commands.iter().find_map(|cmd| match cmd {
            LoadCommand::Dysymtab(d) => Some(*d),
            _ => None,
        })
    }

    pub fn dyld_info(&self) -> Option<DyldInfo> {
        self.commands.iter().find_map(|cmd| match cmd {
            LoadCommand::DyldInfo(d) => Some(*d),
            _ => None,
        })
    }

    pub fn chained_fixups(&self) -> Option<LinkeditData> {
        self.commands.iter().find_map(|cmd| match cmd {
            LoadCommand::ChainedFixups(d) => Some(*d),
            _ => None,
        })
    }

    /// `LC_FUNCTION_STARTS`, whose payload is decoded by
    /// [`function_starts`](crate::image::function_starts).
    pub fn function_starts_data(&self) -> Option<LinkeditData> {
        self.commands.iter().find_map(|cmd| match cmd {
            LoadCommand::FunctionStarts(d) => Some(*d),
            _ => None,
        })
    }

    pub fn exports_trie(&self) -> Option<LinkeditData> {
        self.commands.iter().find_map(|cmd| match cmd {
            LoadCommand::ExportsTrie(d) => Some(*d),
            _ => None,
        })
    }

    /// Reject an image whose `__TEXT` the kernel would have to decrypt for us.
    pub fn check_not_encrypted(&self) -> Result<()> {
        for cmd in &self.commands {
            if let LoadCommand::EncryptionInfo(e) = cmd
                && e.cryptid != 0
            {
                return Err(Error::Encrypted);
            }
        }
        Ok(())
    }

    /// Translate a virtual address to an offset within the slice.
    ///
    /// Returns `None` for addresses in zero-fill regions, which have no file
    /// bytes, and for addresses outside every segment.
    pub fn vmaddr_to_off(&self, vmaddr: Word) -> Option<usize> {
        let seg = self.segments.iter().find(|s| s.contains(vmaddr))?;
        let delta = vmaddr - seg.vmaddr;
        if delta >= seg.filesize {
            return None;
        }
        usize::try_from(seg.fileoff + delta).ok()
    }
}

impl<S: MachSpec> core::fmt::Debug for MachObject<'_, S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MachObject")
            .field("filetype", &self.filetype)
            .field("flags", &format_args!("{:#x}", self.flags))
            .field("segments", &self.segments.len())
            .field("sections", &self.sections.len())
            .field("dependencies", &self.dependencies.len())
            .finish()
    }
}
