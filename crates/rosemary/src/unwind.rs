use machish::image::UnwindEntry;
use microlift::arch::x86_64::context::RegisterContext;

use crate::abi::{AbiError, GuestMemory, Result};
use crate::shims::bind;
use crate::vm::Vm;

/// The callee-saved state a frame restores, plus where it resumes.
///
/// Exactly SysV AMD64's callee-saved set.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    pub rip: u64,
    pub rsp: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

impl Registers {
    /// The frame a live register context is suspended in.
    pub fn from_context(ctx: &RegisterContext) -> Self {
        Registers {
            rip: ctx.rip,
            rsp: ctx.rsp,
            rbx: ctx.rbx,
            rbp: ctx.rbp,
            r12: ctx.r12,
            r13: ctx.r13,
            r14: ctx.r14,
            r15: ctx.r15,
        }
    }

    /// Put a frame back into a register context, leaving everything else.
    pub fn apply(&self, ctx: &mut RegisterContext) {
        ctx.rip = self.rip;
        ctx.rsp = self.rsp;
        ctx.rbx = self.rbx;
        ctx.rbp = self.rbp;
        ctx.r12 = self.r12;
        ctx.r13 = self.r13;
        ctx.r14 = self.r14;
        ctx.r15 = self.r15;
    }

    fn set(&mut self, reg: u8, value: u64) {
        match reg {
            REG_RBX => self.rbx = value,
            REG_R12 => self.r12 = value,
            REG_R13 => self.r13 = value,
            REG_R14 => self.r14 = value,
            REG_R15 => self.r15 = value,
            REG_RBP => self.rbp = value,
            _ => {}
        }
    }
}

// The register numbering the compact encoding uses. Zero means "not saved",
// which is why the set above ignores it rather than treating it as a register.
const REG_RBX: u8 = 1;
const REG_R12: u8 = 2;
const REG_R13: u8 = 3;
const REG_R14: u8 = 4;
const REG_R15: u8 = 5;
const REG_RBP: u8 = 6;

const MODE_MASK: u32 = 0x0f00_0000;
const MODE_RBP_FRAME: u32 = 0x0100_0000;
const MODE_STACK_IMMD: u32 = 0x0200_0000;
const MODE_STACK_IND: u32 = 0x0300_0000;
const MODE_DWARF: u32 = 0x0400_0000;

const RBP_REGISTERS: u32 = 0x0000_7fff;
const RBP_OFFSET: u32 = 0x00ff_0000;
const FRAMELESS_SIZE: u32 = 0x00ff_0000;
const FRAMELESS_ADJUST: u32 = 0x0000_e000;
const FRAMELESS_REG_COUNT: u32 = 0x0000_1c00;
const FRAMELESS_PERMUTATION: u32 = 0x0000_03ff;

/// Anything that stops a stack walk. Every variant is a sentence a user can act
/// on, because an unwinder that fails silently unwinds into nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnwindError {
    /// The function's encoding says "see `__eh_frame`", which is not parsed.
    NeedsDwarf,
    /// No `__unwind_info` entry, or an encoding of zero: nothing describes how
    /// to leave this function.
    NoEntry,
    /// A frame pointer or return address pointed outside anything mapped.
    BadFrame,
    /// The `__gcc_except_tab` entry could not be read, or uses an encoding this
    /// parser does not implement.
    BadLsda,
    /// The walk left the guest without finding a handler.
    NoHandler,
    /// The walk reached the edge of a *nested* entry into guest code.
    AcrossHostFrame,
    /// A handler exists, but deciding it needs type relationships (a base class,
    /// a pointer conversion) that are not implemented.
    NeedsTypeGraph,
}

impl UnwindError {
    /// The sentence that reaches the user, through [`AbiError::Refused`].
    pub fn reason(self) -> &'static str {
        match self {
            UnwindError::NeedsDwarf => {
                "a frame unwinds through __eh_frame, which this runtime does not parse (M5)"
            }
            UnwindError::NoEntry => "a frame has no __unwind_info entry to leave it by",
            UnwindError::BadFrame => "the guest stack does not describe a frame here",
            UnwindError::BadLsda => "the exception table could not be read",
            UnwindError::NoHandler => "no guest frame catches this exception (std::terminate)",
            UnwindError::AcrossHostFrame => {
                "an exception thrown out of a callback the host called, across host frames the \
                 runtime cannot unwind yet (M5)"
            }
            UnwindError::NeedsTypeGraph => {
                "a catch clause needs base-class or pointer-conversion matching, which needs \
                 the guest's type_info graph (M5)"
            }
        }
    }

    fn as_error(self, symbol: &str) -> AbiError {
        AbiError::Refused {
            symbol: symbol.to_string(),
            reason: self.reason(),
        }
    }
}

/// Reads little-endian scalars out of guest memory at a moving cursor.
struct Reader<'a> {
    mem: &'a dyn GuestMemory,
    at: u64,
}

impl<'a> Reader<'a> {
    fn new(mem: &'a dyn GuestMemory, at: u64) -> Self {
        Reader { mem, at }
    }

    fn u8(&mut self) -> Option<u8> {
        let mut b = [0u8; 1];
        self.mem.read(self.at, &mut b).ok()?;
        self.at += 1;
        Some(b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        let mut b = [0u8; 2];
        self.mem.read(self.at, &mut b).ok()?;
        self.at += 2;
        Some(u16::from_le_bytes(b))
    }

    fn u32(&mut self) -> Option<u32> {
        let mut b = [0u8; 4];
        self.mem.read(self.at, &mut b).ok()?;
        self.at += 4;
        Some(u32::from_le_bytes(b))
    }

    fn u64(&mut self) -> Option<u64> {
        let mut b = [0u8; 8];
        self.mem.read(self.at, &mut b).ok()?;
        self.at += 8;
        Some(u64::from_le_bytes(b))
    }

    fn uleb128(&mut self) -> Option<u64> {
        let (mut out, mut shift) = (0u64, 0u32);
        loop {
            let byte = self.u8()?;
            if shift < 64 {
                out |= u64::from(byte & 0x7f) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                return Some(out);
            }
            if shift > 70 {
                return None;
            }
        }
    }

    fn sleb128(&mut self) -> Option<i64> {
        let (mut out, mut shift) = (0i64, 0u32);
        loop {
            let byte = self.u8()?;
            if shift < 64 {
                out |= i64::from(byte & 0x7f) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    out |= -1i64 << shift;
                }
                return Some(out);
            }
            if shift > 70 {
                return None;
            }
        }
    }

    /// A DWARF-encoded pointer, of the subset clang emits for x86-64 Mach-O.
    fn encoded(&mut self, encoding: u8) -> Option<u64> {
        if encoding == DW_EH_PE_OMIT {
            return None;
        }
        let base = self.at;
        let value = match encoding & 0x0f {
            0x00 => self.u64()?,                          // absptr
            0x01 => self.uleb128()?,                      // uleb128
            0x02 => u64::from(self.u16()?),               // udata2
            0x03 => u64::from(self.u32()?),               // udata4
            0x04 => self.u64()?,                          // udata8
            0x09 => self.sleb128()? as u64,               // sleb128
            0x0a => i64::from(self.u16()? as i16) as u64, // sdata2
            0x0b => i64::from(self.u32()? as i32) as u64, // sdata4
            0x0c => self.u64()?,                          // sdata8
            _ => return None,
        };
        // A zero *stored* value is a null pointer, whatever the encoding says
        // about relocating it. This is not a special case invented here.
        if value == 0 {
            return Some(0);
        }
        let value = match encoding & 0x70 {
            0x00 => value,                    // absolute
            0x10 => base.wrapping_add(value), // pcrel
            _ => return None,
        };
        // An indirect encoding stores the address of the address: a GOT slot,
        // because the value is only known after binding.
        if encoding & DW_EH_PE_INDIRECT != 0 {
            if value == 0 {
                return Some(0);
            }
            let mut at = Reader::new(self.mem, value);
            return at.u64();
        }
        Some(value)
    }
}

const DW_EH_PE_OMIT: u8 = 0xff;
const DW_EH_PE_INDIRECT: u8 = 0x80;

/// Leave one frame: `regs` goes in describing frame *n* and comes out
/// describing its caller.
///
/// This is the whole of the x86-64 half of compact unwinding, and every branch
/// of it is a fact about how a function was compiled rather than a heuristic.
pub fn step(
    regs: &mut Registers,
    entry: &UnwindEntry,
    mem: &dyn GuestMemory,
) -> core::result::Result<(), UnwindError> {
    let encoding = entry.encoding;
    if encoding == 0 {
        return Err(UnwindError::NoEntry);
    }
    let read = |at: u64| -> core::result::Result<u64, UnwindError> {
        let mut b = [0u8; 8];
        mem.read(at, &mut b).map_err(|_| UnwindError::BadFrame)?;
        Ok(u64::from_le_bytes(b))
    };

    match encoding & MODE_MASK {
        // The ordinary frame: `push %rbp; mov %rsp,%rbp`.
        MODE_RBP_FRAME => {
            let offset = u64::from((encoding & RBP_OFFSET) >> 16);
            let mut locations = encoding & RBP_REGISTERS;
            let mut at = regs.rbp.wrapping_sub(8 * offset);
            for _ in 0..5 {
                let reg = (locations & 0x7) as u8;
                if reg != 0 {
                    let value = read(at)?;
                    regs.set(reg, value);
                }
                at = at.wrapping_add(8);
                locations >>= 3;
            }
            let rbp = regs.rbp;
            regs.rip = read(rbp.wrapping_add(8))?;
            regs.rsp = rbp.wrapping_add(16);
            regs.rbp = read(rbp)?;
            Ok(())
        }

        // Frameless: the frame is a known number of bytes and the return
        // address sits at the top of it.
        mode @ (MODE_STACK_IMMD | MODE_STACK_IND) => {
            let size_field = u64::from((encoding & FRAMELESS_SIZE) >> 16);
            let adjust = u64::from((encoding & FRAMELESS_ADJUST) >> 13);
            let count = ((encoding & FRAMELESS_REG_COUNT) >> 10) as usize;
            let permutation = encoding & FRAMELESS_PERMUTATION;

            let size = if mode == MODE_STACK_IND {
                let mut r = Reader::new(mem, entry.function.wrapping_add(size_field));
                let immediate = u64::from(r.u32().ok_or(UnwindError::BadFrame)?);
                immediate.wrapping_add(8 * adjust)
            } else {
                8 * size_field
            };

            let saved = regs
                .rsp
                .wrapping_add(size)
                .wrapping_sub(8)
                .wrapping_sub(8 * count as u64);
            let mut at = saved;
            for reg in decode_permutation(permutation, count) {
                let value = read(at)?;
                regs.set(reg, value);
                at = at.wrapping_add(8);
            }
            regs.rip = read(regs.rsp.wrapping_add(size).wrapping_sub(8))?;
            regs.rsp = regs.rsp.wrapping_add(size);
            Ok(())
        }

        MODE_DWARF => Err(UnwindError::NeedsDwarf),
        _ => Err(UnwindError::NoEntry),
    }
}

/// Which registers a frameless function pushed, in the order it pushed them.
///
/// The linker stores the *permutation index* of the register list rather than the list,
/// because up to six registers out of six named values fit in ten bits that way and
/// would not otherwise.
fn decode_permutation(mut permutation: u32, count: usize) -> Vec<u8> {
    let mut ranks = [0u32; 6];
    match count {
        6 => {
            ranks[0] = permutation / 120;
            permutation -= ranks[0] * 120;
            ranks[1] = permutation / 24;
            permutation -= ranks[1] * 24;
            ranks[2] = permutation / 6;
            permutation -= ranks[2] * 6;
            ranks[3] = permutation / 2;
            permutation -= ranks[3] * 2;
            ranks[4] = permutation;
            ranks[5] = 0;
        }
        5 => {
            ranks[0] = permutation / 120;
            permutation -= ranks[0] * 120;
            ranks[1] = permutation / 24;
            permutation -= ranks[1] * 24;
            ranks[2] = permutation / 6;
            permutation -= ranks[2] * 6;
            ranks[3] = permutation / 2;
            permutation -= ranks[3] * 2;
            ranks[4] = permutation;
        }
        4 => {
            ranks[0] = permutation / 60;
            permutation -= ranks[0] * 60;
            ranks[1] = permutation / 12;
            permutation -= ranks[1] * 12;
            ranks[2] = permutation / 3;
            permutation -= ranks[2] * 3;
            ranks[3] = permutation;
        }
        3 => {
            ranks[0] = permutation / 20;
            permutation -= ranks[0] * 20;
            ranks[1] = permutation / 4;
            permutation -= ranks[1] * 4;
            ranks[2] = permutation;
        }
        2 => {
            ranks[0] = permutation / 5;
            permutation -= ranks[0] * 5;
            ranks[1] = permutation;
        }
        1 => ranks[0] = permutation,
        _ => return Vec::new(),
    }

    let mut used = [false; 7];
    let mut out = Vec::with_capacity(count);
    for &rank in ranks.iter().take(count) {
        let mut seen = 0;
        for candidate in 1u8..7 {
            if used[candidate as usize] {
                continue;
            }
            if seen == rank {
                used[candidate as usize] = true;
                out.push(candidate);
                break;
            }
            seen += 1;
        }
    }
    out
}

/// One function's `__gcc_except_tab` entry: where its `try` ranges are and what
/// they catch.
pub struct Lsda {
    /// Base the call-site table's offsets are relative to. Almost always the
    /// function start, which is what `DW_EH_PE_omit` on the field means.
    lp_start: u64,
    ttype_encoding: u8,
    /// One past the end of the type table, which is indexed *backwards*.
    class_info: u64,
    call_site_encoding: u8,
    call_sites: (u64, u64),
    action_table: u64,
}

/// A `try` range's outcome for one address.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Site {
    /// Absolute address of the landing pad, or zero for "nothing here".
    pub landing_pad: u64,
    /// 1-based byte offset into the action table, or zero for a pure cleanup.
    pub action: u64,
}

/// What this frame wants to do about an exception.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A `catch` matched: enter the landing pad with this selector in `rdx`.
    Handler { landing_pad: u64, selector: i64 },
    /// Destructors to run, then `_Unwind_Resume`.
    Cleanup { landing_pad: u64 },
    /// This frame is not involved.
    Nothing,
    /// A catch clause exists whose type could not be compared. See
    /// [`UnwindError::NeedsTypeGraph`].
    Undecidable,
}

impl Lsda {
    /// Parse the header of the table at `lsda`.
    pub fn parse(
        lsda: u64,
        function: u64,
        mem: &dyn GuestMemory,
    ) -> core::result::Result<Self, UnwindError> {
        let mut r = Reader::new(mem, lsda);
        let bad = UnwindError::BadLsda;
        let lp_encoding = r.u8().ok_or(bad)?;
        let lp_start = if lp_encoding == DW_EH_PE_OMIT {
            function
        } else {
            r.encoded(lp_encoding).ok_or(bad)?
        };
        let ttype_encoding = r.u8().ok_or(bad)?;
        let class_info = if ttype_encoding == DW_EH_PE_OMIT {
            0
        } else {
            let offset = r.uleb128().ok_or(bad)?;
            r.at.wrapping_add(offset)
        };
        let call_site_encoding = r.u8().ok_or(bad)?;
        let length = r.uleb128().ok_or(bad)?;
        let start = r.at;
        let end = start.wrapping_add(length);
        Ok(Lsda {
            lp_start,
            ttype_encoding,
            class_info,
            call_site_encoding,
            call_sites: (start, end),
            action_table: end,
        })
    }

    /// The call site covering `ip`, if any.
    pub fn site_for(
        &self,
        ip: u64,
        mem: &dyn GuestMemory,
    ) -> core::result::Result<Option<Site>, UnwindError> {
        let bad = UnwindError::BadLsda;
        let offset = ip.wrapping_sub(1).wrapping_sub(self.lp_start);
        let mut r = Reader::new(mem, self.call_sites.0);
        while r.at < self.call_sites.1 {
            let start = r.encoded(self.call_site_encoding).ok_or(bad)?;
            let length = r.encoded(self.call_site_encoding).ok_or(bad)?;
            let pad = r.encoded(self.call_site_encoding).ok_or(bad)?;
            let action = r.uleb128().ok_or(bad)?;
            if offset >= start && offset < start.wrapping_add(length) {
                return Ok(Some(Site {
                    landing_pad: if pad == 0 {
                        0
                    } else {
                        self.lp_start.wrapping_add(pad)
                    },
                    action,
                }));
            }
        }
        Ok(None)
    }

    /// Walk a site's action chain and decide what this frame does.
    pub fn outcome(
        &self,
        vm: &Vm,
        site: Site,
        thrown_type: u64,
        mem: &dyn GuestMemory,
    ) -> core::result::Result<Outcome, UnwindError> {
        let bad = UnwindError::BadLsda;
        if site.landing_pad == 0 {
            return Ok(Outcome::Nothing);
        }
        if site.action == 0 {
            return Ok(Outcome::Cleanup {
                landing_pad: site.landing_pad,
            });
        }

        let mut at = self.action_table.wrapping_add(site.action - 1);
        let mut undecidable = false;
        let mut cleanup = false;
        loop {
            let mut r = Reader::new(mem, at);
            let filter = r.sleb128().ok_or(bad)?;
            let next_at = r.at;
            let next = r.sleb128().ok_or(bad)?;

            match filter {
                0 => cleanup = true,
                f if f > 0 => {
                    let catch_type = self.catch_type(f, mem)?;
                    // A null entry is `catch (...)`, which takes anything. An
                    // exact address match is the same object.
                    match catches(vm, catch_type, thrown_type, mem) {
                        Match::Yes => {
                            return Ok(Outcome::Handler {
                                landing_pad: site.landing_pad,
                                selector: f,
                            });
                        }
                        Match::No => {}
                        Match::Unknown => undecidable = true,
                    }
                }
                _ => undecidable = true,
            }

            if next == 0 {
                break;
            }
            at = next_at.wrapping_add_signed(next);
        }

        if undecidable {
            // Not "no handler": a clause here might well catch this, and saying
            // so is the difference between a work item and a wrong answer.
            return Ok(Outcome::Undecidable);
        }
        if cleanup {
            return Ok(Outcome::Cleanup {
                landing_pad: site.landing_pad,
            });
        }
        Ok(Outcome::Nothing)
    }

    /// The `type_info` address a 1-based backwards index names.
    fn catch_type(
        &self,
        index: i64,
        mem: &dyn GuestMemory,
    ) -> core::result::Result<u64, UnwindError> {
        let bad = UnwindError::BadLsda;
        if self.class_info == 0 {
            return Err(bad);
        }
        let size = match self.ttype_encoding & 0x0f {
            0x02 | 0x0a => 2,
            0x03 | 0x0b => 4,
            0x00 | 0x04 | 0x0c => 8,
            _ => return Err(bad),
        };
        let at = self.class_info.wrapping_sub(index as u64 * size);
        Reader::new(mem, at).encoded(self.ttype_encoding).ok_or(bad)
    }
}

/// Whether a `catch` clause takes a thrown type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Match {
    Yes,
    No,
    /// The relationship exists or not, and this cannot tell which.
    Unknown,
}

/// Vtable symbols libc++abi exports, one per shape of class the ABI describes.
///
/// The sixteen is never written down here, and deliberately.
const TI_NO_BASES: &str = "__ZTVN10__cxxabiv117__class_type_infoE";
const TI_ONE_BASE: &str = "__ZTVN10__cxxabiv120__si_class_type_infoE";

/// How deep a base chain is followed before calling it a cycle.
const MAX_BASES: usize = 32;

/// Does `catch_type` take something of `thrown_type`?
///
/// Three answers, for the same reason [`crate::abi::table`] gives three.
fn catches(vm: &Vm, catch_type: u64, thrown: u64, mem: &dyn GuestMemory) -> Match {
    if catch_type == 0 {
        return Match::Yes;
    }
    let no_bases = vm.program.resolved_symbol(TI_NO_BASES);
    let one_base = vm.program.resolved_symbol(TI_ONE_BASE);

    let mut at = thrown;
    for _ in 0..MAX_BASES {
        if at == catch_type {
            return Match::Yes;
        }
        let mut r = Reader::new(mem, at);
        let Some(vptr) = r.u64() else {
            return Match::Unknown;
        };
        if Some(vptr) == no_bases {
            return Match::No;
        }
        if Some(vptr) != one_base {
            // A vtable this image never imported.
            return Match::Unknown;
        }
        // `__si_class_type_info` is `{ vtable, name, base }`.
        let Some(base) = Reader::new(mem, at + 16).u64().filter(|b| *b != 0) else {
            return Match::No;
        };
        at = base;
    }
    Match::Unknown
}

/// The exception header this runtime puts in front of a thrown object.
///
/// The Itanium ABI leaves the layout to the implementation.
#[derive(Clone, Copy, Debug)]
struct ExceptionHeader {
    thrown_type: u64,
    destructor: u64,
    handlers: i32,
}

/// Bytes reserved in front of every thrown object.
const HEADER_SIZE: u64 = 32;

impl ExceptionHeader {
    fn write(self, header: u64, mem: &mut dyn GuestMemory) -> Option<()> {
        let mut bytes = [0u8; HEADER_SIZE as usize];
        bytes[0..8].copy_from_slice(&self.thrown_type.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.destructor.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.handlers.to_le_bytes());
        mem.write(header, &bytes).ok()
    }

    fn read(header: u64, mem: &dyn GuestMemory) -> Option<Self> {
        let mut bytes = [0u8; HEADER_SIZE as usize];
        mem.read(header, &mut bytes).ok()?;
        Some(ExceptionHeader {
            thrown_type: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            destructor: u64::from_le_bytes(bytes[8..16].try_into().ok()?),
            handlers: i32::from_le_bytes(bytes[16..20].try_into().ok()?),
        })
    }
}

/// An exception being unwound on this thread.
#[derive(Clone, Copy, Debug)]
pub struct InFlight {
    /// Our header; the guest's object is [`HEADER_SIZE`] bytes past it.
    pub header: u64,
    pub thrown_type: u64,
    /// The frame phase one found a handler in, identified by where it resumes and how
    /// deep its stack is. Both, because a recursive function's frames share a `rip`.
    handler: (u64, u64),
    /// Where phase two has got to: the frame whose landing pad is running.
    at: Registers,
    /// Which VM entry threw. A landing pad in an outer entry would be reached
    /// across host frames - see the module note.
    entry: u64,
}

/// Walk from `regs` to the frame that will catch `thrown_type`.
fn find_handler(
    vm: &Vm,
    mut regs: Registers,
    thrown_type: u64,
    mem: &dyn GuestMemory,
) -> core::result::Result<(u64, u64), UnwindError> {
    let mut undecidable = false;
    for _ in 0..MAX_FRAMES {
        let Some(entry) = vm.program.unwind_entry(regs.rip) else {
            return Err(edge_of_the_walk(undecidable));
        };
        if entry.can_handle()
            && let Some(lsda) = entry.lsda
        {
            let table = Lsda::parse(lsda, entry.function, mem)?;
            if let Some(site) = table.site_for(regs.rip, mem)? {
                match table.outcome(vm, site, thrown_type, mem)? {
                    Outcome::Handler { .. } => return Ok((regs.rip, regs.rsp)),
                    Outcome::Undecidable => undecidable = true,
                    _ => {}
                }
            }
        }
        step(&mut regs, &entry, mem)?;
        if crate::retstub::is_exit(regs.rip) {
            return Err(edge_of_the_walk(undecidable));
        }
    }
    Err(UnwindError::BadFrame)
}

/// What it means that the walk ran out of guest frames.
///
/// Three different failures share this edge, and telling them apart is the difference
/// between a bug report and a work item.
fn edge_of_the_walk(undecidable: bool) -> UnwindError {
    let nested = crate::thread::current().is_some_and(|s| s.depth.get() > 1);
    if nested {
        UnwindError::AcrossHostFrame
    } else if undecidable {
        UnwindError::NeedsTypeGraph
    } else {
        UnwindError::NoHandler
    }
}

/// How many frames a walk will follow before calling the stack corrupt.
///
/// Totality.
const MAX_FRAMES: usize = 1024;

/// Advance phase two to the next frame with something to run, and enter it.
fn enter_next_pad(
    vm: &Vm,
    flight: &mut InFlight,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
    first: bool,
) -> core::result::Result<bool, UnwindError> {
    let mut regs = flight.at;
    if !first {
        let entry = vm
            .program
            .unwind_entry(regs.rip)
            .ok_or(UnwindError::NoEntry)?;
        step(&mut regs, &entry, mem)?;
    }

    for _ in 0..MAX_FRAMES {
        if crate::retstub::is_exit(regs.rip) {
            return Ok(false);
        }
        let Some(entry) = vm.program.unwind_entry(regs.rip) else {
            return Ok(false);
        };
        let is_handler_frame = (regs.rip, regs.rsp) == flight.handler;

        if let Some(lsda) = entry.lsda.filter(|_| entry.can_handle()) {
            let table = Lsda::parse(lsda, entry.function, mem)?;
            if let Some(site) = table.site_for(regs.rip, mem)?
                && let outcome = table.outcome(vm, site, flight.thrown_type, mem)?
                && let Some(pad) = pad_of(outcome)
            {
                flight.at = regs;
                regs.apply(ctx);
                // The landing pad's contract with its caller.
                ctx.rax = flight.header;
                ctx.rdx = match outcome {
                    Outcome::Handler { selector, .. } => selector as u64,
                    _ => 0,
                };
                ctx.rip = pad;
                return Ok(true);
            }
        }
        if is_handler_frame {
            // Phase one said this frame catches and phase two disagrees, which
            // means the two walks read different tables. Better to stop.
            return Err(UnwindError::BadLsda);
        }
        step(&mut regs, &entry, mem)?;
    }
    Err(UnwindError::BadFrame)
}

fn pad_of(outcome: Outcome) -> Option<u64> {
    match outcome {
        Outcome::Handler { landing_pad, .. } | Outcome::Cleanup { landing_pad } => {
            Some(landing_pad)
        }
        _ => None,
    }
}

/// `void *__cxa_allocate_exception(size_t)`
///
/// Host memory, which the identity mapping makes guest-addressable.
pub fn allocate_exception(
    symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let size = ctx.rdi;
    let total = size.checked_add(HEADER_SIZE).ok_or(AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "an exception larger than the address space",
    })?;
    // SAFETY: an ordinary allocation, sized to hold the runtime's header and the
    // guest's object. Freed by `__cxa_end_catch` or `__cxa_free_exception`.
    let block = unsafe { libc::malloc(total as usize) } as u64;
    if block == 0 {
        return Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "out of memory allocating an exception",
        });
    }
    ctx.rax = block + HEADER_SIZE;
    Ok(())
}

/// `void __cxa_free_exception(void *)`
pub fn free_exception(
    _symbol: &str,
    ctx: &mut RegisterContext,
    _mem: &mut dyn GuestMemory,
) -> Result<()> {
    let object = ctx.rdi;
    if object >= HEADER_SIZE {
        // SAFETY: the guest hands back exactly what `allocate_exception`
        // returned, so the block is this module's own allocation.
        unsafe { libc::free((object - HEADER_SIZE) as *mut libc::c_void) };
    }
    Ok(())
}

/// `void __cxa_throw(void *object, std::type_info *type, void (*dtor)(void *))`
///
/// Never returns to its caller: on success `ctx` has been moved to the first
/// landing pad of the unwind.
pub fn throw(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let vm = current_vm(symbol)?;
    let state = crate::thread::current().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "a throw outside a running guest thread",
    })?;
    if state.in_flight().is_some() {
        return Err(AbiError::Refused {
            symbol: symbol.to_string(),
            reason: "a second exception thrown while one is being unwound (M5)",
        });
    }

    let (object, thrown_type, destructor) = (ctx.rdi, ctx.rsi, ctx.rdx);
    let header = object.wrapping_sub(HEADER_SIZE);
    ExceptionHeader {
        thrown_type,
        destructor,
        handlers: 0,
    }
    .write(header, mem)
    .ok_or(AbiError::Memory {
        symbol: symbol.to_string(),
        addr: header,
    })?;

    // The frame that threw, as it will be unwound from.
    let regs = Registers::from_context(ctx);
    let handler = find_handler(vm, regs, thrown_type, mem).map_err(|e| e.as_error(symbol))?;

    let mut flight = InFlight {
        header,
        thrown_type,
        handler,
        at: regs,
        entry: state.entry(),
    };
    let entered =
        enter_next_pad(vm, &mut flight, ctx, mem, true).map_err(|e| e.as_error(symbol))?;
    if !entered {
        return Err(UnwindError::NoHandler.as_error(symbol));
    }
    state.set_in_flight(Some(flight));
    state.rsp.set(ctx.rsp);
    state.redirect();
    Ok(())
}

/// `void _Unwind_Resume(_Unwind_Exception *)`
pub fn resume(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let vm = current_vm(symbol)?;
    let state = crate::thread::current().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "a resume outside a running guest thread",
    })?;
    let mut flight = state.in_flight().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "_Unwind_Resume with no exception in flight",
    })?;
    if flight.entry != state.entry() {
        return Err(UnwindError::NoHandler.as_error(symbol));
    }

    let entered =
        enter_next_pad(vm, &mut flight, ctx, mem, false).map_err(|e| e.as_error(symbol))?;
    if !entered {
        state.set_in_flight(None);
        return Err(UnwindError::NoHandler.as_error(symbol));
    }
    state.set_in_flight(Some(flight));
    state.rsp.set(ctx.rsp);
    state.redirect();
    Ok(())
}

/// `void *__cxa_begin_catch(_Unwind_Exception *)`
pub fn begin_catch(
    symbol: &str,
    ctx: &mut RegisterContext,
    mem: &mut dyn GuestMemory,
) -> Result<()> {
    let header = ctx.rdi;
    let mut record = ExceptionHeader::read(header, mem).ok_or(AbiError::Memory {
        symbol: symbol.to_string(),
        addr: header,
    })?;
    record.handlers += 1;
    record.write(header, mem).ok_or(AbiError::Memory {
        symbol: symbol.to_string(),
        addr: header,
    })?;
    if let Some(state) = crate::thread::current() {
        state.begin_catch(header);
        if state.in_flight().is_some_and(|f| f.header == header) {
            state.set_in_flight(None);
        }
    }
    ctx.rax = header.wrapping_add(HEADER_SIZE);
    Ok(())
}

/// `void __cxa_rethrow()`
pub fn rethrow(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let vm = current_vm(symbol)?;
    let state = crate::thread::current().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "a rethrow outside a running guest thread",
    })?;
    let header = state.caught().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "a rethrow with no exception being handled",
    })?;
    let record = ExceptionHeader::read(header, mem).ok_or(AbiError::Memory {
        symbol: symbol.to_string(),
        addr: header,
    })?;

    let regs = Registers::from_context(ctx);
    let handler =
        find_handler(vm, regs, record.thrown_type, mem).map_err(|e| e.as_error(symbol))?;
    let mut flight = InFlight {
        header,
        thrown_type: record.thrown_type,
        handler,
        at: regs,
        entry: state.entry(),
    };
    let entered =
        enter_next_pad(vm, &mut flight, ctx, mem, true).map_err(|e| e.as_error(symbol))?;
    if !entered {
        return Err(UnwindError::NoHandler.as_error(symbol));
    }
    state.set_in_flight(Some(flight));
    state.rsp.set(ctx.rsp);
    state.redirect();
    Ok(())
}

/// `void __cxa_end_catch()`
pub fn end_catch(symbol: &str, ctx: &mut RegisterContext, mem: &mut dyn GuestMemory) -> Result<()> {
    let vm = current_vm(symbol)?;
    let state = crate::thread::current().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "an end_catch outside a running guest thread",
    })?;
    // `__cxa_end_catch` is emitted on paths where nothing was caught - the
    // cleanup edge of a `try` whose handler never ran - so having no exception
    // to end is ordinary rather than an error.
    let Some(header) = state.end_catch() else {
        return Ok(());
    };
    let Some(mut record) = ExceptionHeader::read(header, mem) else {
        return Err(AbiError::Memory {
            symbol: symbol.to_string(),
            addr: header,
        });
    };

    record.handlers -= 1;
    record.write(header, mem).ok_or(AbiError::Memory {
        symbol: symbol.to_string(),
        addr: header,
    })?;

    // A rethrown exception is being unwound *right now*, and this call is the
    // cleanup edge of the `catch` block it left.
    if state.in_flight().is_some_and(|f| f.header == header) || record.handlers > 0 {
        ctx.rax = 0;
        return Ok(());
    }

    if record.destructor != 0 {
        let object = header.wrapping_add(HEADER_SIZE);
        vm.call(record.destructor, &[object])
            .map_err(|e| AbiError::Nested {
                symbol: symbol.to_string(),
                reason: format!("the thrown object's destructor could not be run: {e}"),
            })?;
    }
    // SAFETY: `header` is what `allocate_exception` returned, minus the header
    // it added, and nothing can reach it now: the catch that owned it is over
    // and no walk is using it.
    unsafe { libc::free(header as *mut libc::c_void) };
    ctx.rax = 0;
    Ok(())
}

/// The VM running this thread, which every unwinding shim needs for the guest's
/// tables and for entering its code.
fn current_vm(symbol: &str) -> Result<&'static Vm> {
    crate::thunk::current().ok_or_else(|| AbiError::Refused {
        symbol: symbol.to_string(),
        reason: "unwinding needs the JIT, and no VM is running on this thread",
    })
}

/// Whether `name` (underscore already stripped) is one of this module's.
pub fn shim_for(name: &str) -> Option<crate::jmpbuf::Shim> {
    Some(match name {
        "__cxa_allocate_exception" => bind!("__cxa_allocate_exception" => allocate_exception),
        "__cxa_free_exception" => bind!("__cxa_free_exception" => free_exception),
        "__cxa_throw" => bind!("__cxa_throw" => throw),
        "__cxa_rethrow" => bind!("__cxa_rethrow" => rethrow),
        "__cxa_begin_catch" => bind!("__cxa_begin_catch" => begin_catch),
        "__cxa_end_catch" => bind!("__cxa_end_catch" => end_catch),
        "_Unwind_Resume" => bind!("_Unwind_Resume" => resume),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stretch of guest stack at a known address.
    struct Stack {
        base: u64,
        bytes: Vec<u8>,
    }

    impl Stack {
        fn new(base: u64, len: usize) -> Self {
            Stack {
                base,
                bytes: vec![0; len],
            }
        }

        fn put(&mut self, at: u64, value: u64) {
            let off = (at - self.base) as usize;
            self.bytes[off..off + 8].copy_from_slice(&value.to_le_bytes());
        }
    }

    impl GuestMemory for Stack {
        fn read(&self, va: u64, out: &mut [u8]) -> core::result::Result<(), machish::Error> {
            let off = va.wrapping_sub(self.base) as usize;
            match off
                .checked_add(out.len())
                .filter(|e| *e <= self.bytes.len())
            {
                Some(end) => {
                    out.copy_from_slice(&self.bytes[off..end]);
                    Ok(())
                }
                None => Err(machish::Error::OutOfBounds {
                    addr: va,
                    len: out.len(),
                }),
            }
        }

        fn write(&mut self, va: u64, data: &[u8]) -> core::result::Result<(), machish::Error> {
            let off = va.wrapping_sub(self.base) as usize;
            match off
                .checked_add(data.len())
                .filter(|e| *e <= self.bytes.len())
            {
                Some(end) => {
                    self.bytes[off..end].copy_from_slice(data);
                    Ok(())
                }
                None => Err(machish::Error::OutOfBounds {
                    addr: va,
                    len: data.len(),
                }),
            }
        }
    }

    fn entry(encoding: u32) -> UnwindEntry {
        UnwindEntry {
            function: 0x1_0000_0000,
            encoding,
            personality_slot: None,
            lsda: None,
        }
    }

    #[test]
    fn an_rbp_frame_yields_its_callers_frame() {
        // The layout `push %rbp; mov %rsp,%rbp` produces.
        let mut stack = Stack::new(0x7000, 0x200);
        let (rbp, caller_rbp) = (0x7100u64, 0x7180u64);
        stack.put(rbp, caller_rbp);
        stack.put(rbp + 8, 0x1_0000_1234);

        let mut regs = Registers {
            rip: 0x1_0000_5000,
            rsp: 0x70e0,
            rbp,
            ..Default::default()
        };
        step(&mut regs, &entry(MODE_RBP_FRAME), &stack).expect("step");

        assert_eq!(regs.rip, 0x1_0000_1234, "the return address is at [rbp+8]");
        assert_eq!(regs.rbp, caller_rbp);
        assert_eq!(
            regs.rsp,
            rbp + 16,
            "the caller's rsp is past both saved words"
        );
    }

    #[test]
    fn an_rbp_frame_restores_the_registers_it_spilled() {
        // Two registers spilled at `rbp - 8*offset`, in the order the three-bit
        // slots name them.
        let mut stack = Stack::new(0x7000, 0x200);
        let rbp = 0x7100u64;
        stack.put(rbp, 0x7180);
        stack.put(rbp + 8, 0x1_0000_1234);
        stack.put(rbp - 16, 0xbbbb); // first slot
        stack.put(rbp - 8, 0xcccc); // second slot

        // offset 2 (registers start at rbp-16), slots: RBX then R12.
        let encoding = MODE_RBP_FRAME | (2 << 16) | u32::from(REG_RBX) | (u32::from(REG_R12) << 3);
        let mut regs = Registers {
            rip: 0x1_0000_5000,
            rsp: 0x70e0,
            rbp,
            ..Default::default()
        };
        step(&mut regs, &entry(encoding), &stack).expect("step");

        assert_eq!(regs.rbx, 0xbbbb);
        assert_eq!(regs.r12, 0xcccc);
    }

    #[test]
    fn a_frameless_function_is_left_by_its_recorded_size() {
        // No frame pointer: the return address is at the top of a frame whose
        // size the encoding states in words.
        let mut stack = Stack::new(0x7000, 0x200);
        let rsp = 0x7100u64;
        let size = 4 * 8;
        stack.put(rsp + size - 8, 0x1_0000_9999);

        let encoding = MODE_STACK_IMMD | (4 << 16);
        let mut regs = Registers {
            rip: 0x1_0000_5000,
            rsp,
            ..Default::default()
        };
        step(&mut regs, &entry(encoding), &stack).expect("step");

        assert_eq!(regs.rip, 0x1_0000_9999);
        assert_eq!(regs.rsp, rsp + size);
    }

    #[test]
    fn a_frameless_function_restores_pushed_registers_in_order() {
        let mut stack = Stack::new(0x7000, 0x200);
        let rsp = 0x7100u64;
        let size = 6 * 8;
        stack.put(rsp + size - 8, 0x1_0000_9999);
        // Two registers, immediately below the return address.
        stack.put(rsp + size - 24, 0x1111);
        stack.put(rsp + size - 16, 0x2222);

        // Permutation 0 of two registers is the first two in the numbering:
        // RBX and R12.
        let encoding = MODE_STACK_IMMD | (6 << 16) | (2 << 10);
        let mut regs = Registers {
            rip: 0x1_0000_5000,
            rsp,
            ..Default::default()
        };
        step(&mut regs, &entry(encoding), &stack).expect("step");

        assert_eq!((regs.rbx, regs.r12), (0x1111, 0x2222));
        assert_eq!(regs.rip, 0x1_0000_9999);
    }

    #[test]
    fn a_dwarf_encoding_is_refused_rather_than_guessed_at() {
        let stack = Stack::new(0x7000, 0x100);
        let mut regs = Registers::default();
        assert_eq!(
            step(&mut regs, &entry(MODE_DWARF), &stack),
            Err(UnwindError::NeedsDwarf)
        );
        assert_eq!(
            step(&mut regs, &entry(0), &stack),
            Err(UnwindError::NoEntry)
        );
        // And the guest's registers are untouched by a refusal.
        assert_eq!(regs, Registers::default());
    }

    #[test]
    fn a_wild_frame_pointer_ends_the_walk_rather_than_the_process() {
        let stack = Stack::new(0x7000, 0x100);
        let mut regs = Registers {
            rbp: 0xdead_beef,
            ..Default::default()
        };
        assert_eq!(
            step(&mut regs, &entry(MODE_RBP_FRAME), &stack),
            Err(UnwindError::BadFrame)
        );
    }

    #[test]
    fn the_permutation_decodes_to_distinct_registers() {
        // The property that matters.
        for count in 1..=6usize {
            for permutation in 0..720u32 {
                let regs = decode_permutation(permutation, count);
                if regs.len() != count {
                    continue;
                }
                let mut seen = regs.clone();
                seen.sort_unstable();
                seen.dedup();
                assert_eq!(
                    seen.len(),
                    count,
                    "permutation {permutation} of {count} repeats"
                );
                assert!(regs.iter().all(|r| (1..=6).contains(r)));
            }
        }
        // Index zero is the identity: the first `count` registers in order.
        assert_eq!(decode_permutation(0, 3), vec![REG_RBX, REG_R12, REG_R13]);
    }

    #[test]
    fn an_uncatchable_exception_is_told_apart_from_an_unmatched_one() {
        // The two failures a search can end in, and they are different work
        // items.
        assert!(UnwindError::NoHandler.reason().contains("terminate"));
        assert!(UnwindError::NeedsTypeGraph.reason().contains("type_info"));
        assert_ne!(UnwindError::NoHandler, UnwindError::NeedsTypeGraph);
    }
}
