//! One function's `__gcc_except_tab` entry: its `try` ranges, their landing pads,
//! and the action and type tables a personality routine walks.
//!
//! Read through guest memory rather than out of the file because the type table
//! holds pointers that binding fills in.

use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::memory::AddressSpace;
use crate::raw::Word;

const DW_EH_PE_OMIT: u8 = 0xff;
const DW_EH_PE_INDIRECT: u8 = 0x80;

const DW_EH_PE_FORMAT: u8 = 0x0f;
const DW_EH_PE_ABSPTR: u8 = 0x00;
const DW_EH_PE_ULEB128: u8 = 0x01;
const DW_EH_PE_UDATA2: u8 = 0x02;
const DW_EH_PE_UDATA4: u8 = 0x03;
const DW_EH_PE_UDATA8: u8 = 0x04;
const DW_EH_PE_SLEB128: u8 = 0x09;
const DW_EH_PE_SDATA2: u8 = 0x0a;
const DW_EH_PE_SDATA4: u8 = 0x0b;
const DW_EH_PE_SDATA8: u8 = 0x0c;

const DW_EH_PE_APPLICATION: u8 = 0x70;
const DW_EH_PE_ABSOLUTE: u8 = 0x00;
const DW_EH_PE_PCREL: u8 = 0x10;

/// The longest LEB128 a 64-bit value can need, plus the one byte of slack an
/// encoder may leave.
const MAX_LEB_SHIFT: u32 = 70;

const BAD: Error = Error::BadExceptionTable;

/// Memory an exception table can be read from.
pub trait LsdaMemory {
    fn read(&self, addr: Word, out: &mut [u8]) -> Result<()>;
}

impl<T: AddressSpace + ?Sized> LsdaMemory for T {
    fn read(&self, addr: Word, out: &mut [u8]) -> Result<()> {
        AddressSpace::read(self, addr, out)
    }
}

/// A parsed table header. Every address in it is slid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lsda {
    /// Base the call-site table's offsets are relative to. Almost always the
    /// function start, which is what `DW_EH_PE_omit` on the field means.
    pub lp_start: Word,
    pub ttype_encoding: u8,
    /// One past the end of the type table, which is indexed *backwards*; zero
    /// when there is none.
    pub class_info: Word,
    pub call_site_encoding: u8,
    pub call_sites: core::ops::Range<Word>,
    pub action_table: Word,
}

/// One `try` range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallSite {
    /// Slid addresses the range covers.
    pub range: core::ops::Range<Word>,
    /// Absolute address of the landing pad, or zero for "nothing here".
    pub landing_pad: Word,
    /// 1-based byte offset into the action table, or zero for a pure cleanup.
    pub action: u64,
}

/// One entry of an action chain.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Action {
    /// Positive: a 1-based type-table index. Zero: a cleanup. Negative: an
    /// exception specification.
    pub filter: i64,
    /// Where the next entry is, or `None` at the end of the chain.
    pub next: Option<Word>,
}

impl Lsda {
    /// Parse the header of the table at `lsda`, which belongs to `function`.
    pub fn parse<M: LsdaMemory + ?Sized>(lsda: Word, function: Word, mem: &M) -> Result<Self> {
        let mut r = Reader::new(mem, lsda);
        let lp_encoding = r.u8()?;
        let lp_start = if lp_encoding == DW_EH_PE_OMIT {
            function
        } else {
            r.encoded(lp_encoding)?
        };
        let ttype_encoding = r.u8()?;
        let class_info = if ttype_encoding == DW_EH_PE_OMIT {
            0
        } else {
            let offset = r.uleb128()?;
            r.at.wrapping_add(offset)
        };
        let call_site_encoding = r.u8()?;
        let length = r.uleb128()?;
        let start = r.at;
        let end = start.wrapping_add(length);
        Ok(Lsda {
            lp_start,
            ttype_encoding,
            class_info,
            call_site_encoding,
            call_sites: start..end,
            action_table: end,
        })
    }

    /// Every `try` range, in table order.
    pub fn call_sites<M: LsdaMemory + ?Sized>(&self, mem: &M) -> Result<Vec<CallSite>> {
        let mut r = Reader::new(mem, self.call_sites.start);
        let mut out = Vec::new();
        while r.at < self.call_sites.end {
            let start = r.encoded(self.call_site_encoding)?;
            let length = r.encoded(self.call_site_encoding)?;
            let pad = r.encoded(self.call_site_encoding)?;
            let action = r.uleb128()?;
            let start = self.lp_start.wrapping_add(start);
            out.push(CallSite {
                range: start..start.wrapping_add(length),
                landing_pad: if pad == 0 {
                    0
                } else {
                    self.lp_start.wrapping_add(pad)
                },
                action,
            });
        }
        Ok(out)
    }

    /// The call site covering a frame whose return address is `ip`, if any.
    ///
    /// `ip - 1`, because a return address is one past the call that made it.
    pub fn site_for<M: LsdaMemory + ?Sized>(&self, ip: Word, mem: &M) -> Result<Option<CallSite>> {
        let at = ip.wrapping_sub(1);
        Ok(self
            .call_sites(mem)?
            .into_iter()
            .find(|site| site.range.contains(&at)))
    }

    /// Every distinct landing pad, ascending.
    pub fn landing_pads<M: LsdaMemory + ?Sized>(&self, mem: &M) -> Result<Vec<Word>> {
        let mut pads: Vec<Word> = self
            .call_sites(mem)?
            .into_iter()
            .map(|site| site.landing_pad)
            .filter(|&pad| pad != 0)
            .collect();
        pads.sort_unstable();
        pads.dedup();
        Ok(pads)
    }

    /// Where `site`'s action chain starts, or `None` for a pure cleanup.
    pub fn first_action(&self, site: &CallSite) -> Option<Word> {
        (site.action != 0).then(|| self.action_table.wrapping_add(site.action - 1))
    }

    /// The action-chain entry at `at`.
    pub fn action<M: LsdaMemory + ?Sized>(&self, at: Word, mem: &M) -> Result<Action> {
        let mut r = Reader::new(mem, at);
        let filter = r.sleb128()?;
        let next_at = r.at;
        let next = r.sleb128()?;
        Ok(Action {
            filter,
            next: (next != 0).then(|| next_at.wrapping_add_signed(next)),
        })
    }

    /// The `type_info` address a positive filter names; zero is `catch (...)`.
    pub fn catch_type<M: LsdaMemory + ?Sized>(&self, filter: i64, mem: &M) -> Result<Word> {
        if self.class_info == 0 || filter <= 0 {
            return Err(BAD);
        }
        let size: u64 = match self.ttype_encoding & DW_EH_PE_FORMAT {
            DW_EH_PE_UDATA2 | DW_EH_PE_SDATA2 => 2,
            DW_EH_PE_UDATA4 | DW_EH_PE_SDATA4 => 4,
            DW_EH_PE_ABSPTR | DW_EH_PE_UDATA8 | DW_EH_PE_SDATA8 => 8,
            _ => return Err(BAD),
        };
        let at = self.class_info.wrapping_sub(filter as u64 * size);
        Reader::new(mem, at).encoded(self.ttype_encoding)
    }
}

struct Reader<'a, M: ?Sized> {
    mem: &'a M,
    at: Word,
}

impl<'a, M: LsdaMemory + ?Sized> Reader<'a, M> {
    fn new(mem: &'a M, at: Word) -> Self {
        Reader { mem, at }
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut b = [0u8; N];
        self.mem.read(self.at, &mut b).map_err(|_| BAD)?;
        self.at = self.at.wrapping_add(N as u64);
        Ok(b)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes()?))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }

    fn uleb128(&mut self) -> Result<u64> {
        let (mut out, mut shift) = (0u64, 0u32);
        loop {
            let byte = self.u8()?;
            if shift < 64 {
                out |= u64::from(byte & 0x7f) << shift;
            }
            shift += 7;
            if byte & 0x80 == 0 {
                return Ok(out);
            }
            if shift > MAX_LEB_SHIFT {
                return Err(BAD);
            }
        }
    }

    fn sleb128(&mut self) -> Result<i64> {
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
                return Ok(out);
            }
            if shift > MAX_LEB_SHIFT {
                return Err(BAD);
            }
        }
    }

    /// A DWARF-encoded pointer, of the subset clang emits for x86-64 Mach-O.
    fn encoded(&mut self, encoding: u8) -> Result<u64> {
        if encoding == DW_EH_PE_OMIT {
            return Err(BAD);
        }
        let base = self.at;
        let value = match encoding & DW_EH_PE_FORMAT {
            DW_EH_PE_ABSPTR | DW_EH_PE_UDATA8 | DW_EH_PE_SDATA8 => self.u64()?,
            DW_EH_PE_ULEB128 => self.uleb128()?,
            DW_EH_PE_UDATA2 => u64::from(self.u16()?),
            DW_EH_PE_UDATA4 => u64::from(self.u32()?),
            DW_EH_PE_SLEB128 => self.sleb128()? as u64,
            DW_EH_PE_SDATA2 => i64::from(self.u16()? as i16) as u64,
            DW_EH_PE_SDATA4 => i64::from(self.u32()? as i32) as u64,
            _ => return Err(BAD),
        };
        // A zero *stored* value is a null pointer, whatever the encoding says
        // about relocating it.
        if value == 0 {
            return Ok(0);
        }
        let value = match encoding & DW_EH_PE_APPLICATION {
            DW_EH_PE_ABSOLUTE => value,
            DW_EH_PE_PCREL => base.wrapping_add(value),
            _ => return Err(BAD),
        };
        // An indirect encoding stores the address of the address: a GOT slot,
        // because the value is only known after binding.
        if encoding & DW_EH_PE_INDIRECT != 0 {
            return Reader::new(self.mem, value).u64();
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat byte array at a fixed base.
    struct Flat {
        base: Word,
        bytes: Vec<u8>,
    }

    impl LsdaMemory for Flat {
        fn read(&self, addr: Word, out: &mut [u8]) -> Result<()> {
            let start = addr.checked_sub(self.base).ok_or(Error::OutOfBounds {
                addr,
                len: out.len(),
            })? as usize;
            let src = self
                .bytes
                .get(start..start + out.len())
                .ok_or(Error::OutOfBounds {
                    addr,
                    len: out.len(),
                })?;
            out.copy_from_slice(src);
            Ok(())
        }
    }

    const FUNCTION: Word = 0x1000;
    const TABLE: Word = 0x2000;

    /// No type table; two `udata4` call sites, one with a pad and one without.
    fn table() -> Flat {
        let mut bytes = alloc::vec![DW_EH_PE_OMIT, DW_EH_PE_OMIT, DW_EH_PE_UDATA4, 26];
        for (start, len, pad, action) in [(0x10u32, 0x8u32, 0x40u32, 0u8), (0x20, 0x4, 0, 0)] {
            bytes.extend_from_slice(&start.to_le_bytes());
            bytes.extend_from_slice(&len.to_le_bytes());
            bytes.extend_from_slice(&pad.to_le_bytes());
            bytes.push(action);
        }
        Flat { base: TABLE, bytes }
    }

    #[test]
    fn landing_pads_are_relative_to_the_function() {
        let mem = table();
        let lsda = Lsda::parse(TABLE, FUNCTION, &mem).unwrap();
        assert_eq!(lsda.landing_pads(&mem).unwrap(), [FUNCTION + 0x40]);
    }

    #[test]
    fn a_return_address_finds_the_site_of_its_call() {
        let mem = table();
        let lsda = Lsda::parse(TABLE, FUNCTION, &mem).unwrap();
        let site = lsda.site_for(FUNCTION + 0x18, &mem).unwrap().unwrap();
        assert_eq!(site.landing_pad, FUNCTION + 0x40);
        assert_eq!(lsda.first_action(&site), None);
        assert_eq!(lsda.site_for(FUNCTION + 0x19, &mem).unwrap(), None);
        let padless = lsda.site_for(FUNCTION + 0x21, &mem).unwrap().unwrap();
        assert_eq!(padless.landing_pad, 0);
    }

    #[test]
    fn a_table_past_mapped_memory_is_refused() {
        let mem = Flat {
            base: TABLE,
            bytes: alloc::vec![DW_EH_PE_OMIT],
        };
        assert_eq!(Lsda::parse(TABLE, FUNCTION, &mem), Err(BAD));
    }
}
