use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::image::MachObject;
use crate::raw::{View, Word};
use crate::spec::MachSpec;

/// Guards against a corrupt count claiming a page holds more than a section can.
const MAX_ENTRIES: usize = 1 << 22;

/// The only version of the section that exists.
const VERSION: u32 = 1;

const KIND_REGULAR: u32 = 2;
const KIND_COMPRESSED: u32 = 3;

/// Bit saying an entry has language-specific data (a C++ `try` table).
const HAS_LSDA: u32 = 0x4000_0000;
/// 1-based index into the personality array, or zero for none.
const PERSONALITY_MASK: u32 = 0x3000_0000;
const PERSONALITY_SHIFT: u32 = 28;

/// How one function unwinds, as the linker recorded it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct UnwindEntry {
    /// Slid VA of the function this describes.
    pub function: Word,
    /// The architecture-specific compact encoding. Interpreting it is the
    /// unwinder's job; see the module note.
    pub encoding: u32,
    /// Slid VA of the pointer to this function's personality routine, or
    /// `None`.
    pub personality_slot: Option<Word>,
    /// Slid VA of the language-specific data area, or `None`.
    pub lsda: Option<Word>,
}

impl UnwindEntry {
    /// Whether this function has a personality routine and a `try` table,
    /// which together are what make it capable of catching anything.
    pub fn can_handle(&self) -> bool {
        self.personality_slot.is_some() && self.lsda.is_some()
    }
}

/// The parsed `__unwind_info` of one image.
///
/// Held as a flat, address-sorted vector rather than as the two-level structure
/// on disk. The compression exists to make the *file* small; a runtime that has
/// already paid to parse it wants one binary search, and an image's function
/// count is bounded by its `__text`.
#[derive(Clone, Debug, Default)]
pub struct UnwindInfo {
    entries: Vec<UnwindEntry>,
    /// Where `__text` ends, so a lookup past the last function fails rather than
    /// attributing the whole rest of the address space to it.
    end: Word,
}

impl UnwindInfo {
    pub fn parse<S: MachSpec>(obj: &MachObject<'_, S>, slide: Word) -> Result<Self> {
        let Some(sect) = obj.section("__TEXT", "__unwind_info") else {
            return Ok(UnwindInfo::default());
        };
        let start = usize::try_from(sect.offset).map_err(|_| Error::Truncated { what: SECT })?;
        let len = usize::try_from(sect.size).map_err(|_| Error::Truncated { what: SECT })?;
        let view = obj.view().subview(start, len, SECT)?;

        let version = view.u32(0)?;
        if version != VERSION {
            return Err(Error::Custom("unsupported __unwind_info version"));
        }
        let common_off = view.u32(4)? as usize;
        let common_count = view.u32(8)? as usize;
        let personalities_off = view.u32(12)? as usize;
        let personalities_count = view.u32(16)? as usize;
        let index_off = view.u32(20)? as usize;
        let index_count = view.u32(24)? as usize;

        // The last first-level entry is a sentinel: it names the end of the
        // covered range and has no second-level page. Fewer than two entries
        // therefore means nothing is described.
        if index_count < 2 {
            return Ok(UnwindInfo::default());
        }
        if index_count > MAX_ENTRIES || common_count > MAX_ENTRIES {
            return Err(Error::TooDeep(SECT));
        }

        let base = obj.preferred_base()?;
        let personalities = personality_slots(&view, personalities_off, personalities_count, base)?;
        let commons = encodings(&view, common_off, common_count)?;

        let mut entries = Vec::new();
        let mut end = base;
        for i in 0..index_count - 1 {
            let at = index_off + i * 12;
            let function_offset = view.u32(at)? as Word;
            let page_off = view.u32(at + 4)? as usize;
            let lsda_off = view.u32(at + 8)? as usize;
            let next_function_offset = view.u32(at + 12)? as Word;
            let next_lsda_off = view.u32(at + 20)? as usize;
            end = base.wrapping_add(next_function_offset).wrapping_add(slide);

            // A first-level entry with no page is the sentinel's neighbour in a
            // table that covers a gap: nothing to describe, not an error.
            if page_off == 0 {
                continue;
            }
            let lsdas = lsda_index(&view, lsda_off, next_lsda_off, base)?;
            read_page(
                &view,
                page_off,
                base,
                base.wrapping_add(function_offset),
                &commons,
                &personalities,
                &lsdas,
                slide,
                &mut entries,
            )?;
        }

        entries.sort_unstable_by_key(|e| e.function);
        Ok(UnwindInfo { entries, end })
    }

    /// Find the entry covering `va`.
    ///
    /// **`va > end`, not `>=`.** A caller here is a *return address*, so it is
    /// the instruction **after** a `call`. When the last function in the image
    /// ends in a `call`, that return address is exactly `end` - one past the
    /// final instruction - and the call site at `end - 1` is unambiguously in
    /// the last entry. Rejecting `va == end` therefore drops the frame for the
    /// last function in the image, and an unwinder that loses a frame is worse
    /// than one that gains one: it resumes the *caller's* landing pad with this
    /// frame's context, which is a silent wrong answer rather than a refusal.
    ///
    /// Measured: a fixture built by `test/tools/build-macho.sh` put a `call` as
    /// the final instruction, and `lookup` returned `None` for it.
    pub fn lookup(&self, va: Word) -> Option<&UnwindEntry> {
        if self.entries.is_empty() || va > self.end {
            return None;
        }
        let i = match self.entries.binary_search_by_key(&va, |e| e.function) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };
        self.entries.get(i)
    }

    /// Every entry, in ascending address order.
    pub fn entries(&self) -> &[UnwindEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

const SECT: &str = "__unwind_info";

/// The section-wide array of personality GOT slots, as slid VAs.
fn personality_slots(view: &View<'_>, off: usize, count: usize, base: Word) -> Result<Vec<Word>> {
    if count > MAX_ENTRIES {
        return Err(Error::TooDeep(SECT));
    }
    (0..count)
        .map(|i| Ok(base.wrapping_add(view.u32(off + i * 4)? as Word)))
        .collect()
}

fn encodings(view: &View<'_>, off: usize, count: usize) -> Result<Vec<u32>> {
    (0..count).map(|i| view.u32(off + i * 4)).collect()
}

/// The `(function, lsda)` pairs covering one first-level entry.
///
/// The array is shared across the section and sliced by the next first-level
/// entry's offset.
fn lsda_index(view: &View<'_>, from: usize, to: usize, base: Word) -> Result<Vec<(Word, Word)>> {
    if to <= from {
        return Ok(Vec::new());
    }
    let count = (to - from) / 8;
    if count > MAX_ENTRIES {
        return Err(Error::TooDeep(SECT));
    }
    (0..count)
        .map(|i| {
            let at = from + i * 8;
            Ok((
                base.wrapping_add(view.u32(at)? as Word),
                base.wrapping_add(view.u32(at + 4)? as Word),
            ))
        })
        .collect()
}

/// Decode one second-level page, of either kind, appending to `out`.
#[allow(clippy::too_many_arguments)]
fn read_page(
    view: &View<'_>,
    page_off: usize,
    base: Word,
    page_base: Word,
    commons: &[u32],
    personalities: &[Word],
    lsdas: &[(Word, Word)],
    slide: Word,
    out: &mut Vec<UnwindEntry>,
) -> Result<()> {
    let kind = view.u32(page_off)?;
    let (entries_off, count, page_encodings): (usize, usize, Vec<u32>) = match kind {
        KIND_REGULAR => (
            view.u16(page_off + 4)? as usize,
            view.u16(page_off + 6)? as usize,
            Vec::new(),
        ),
        KIND_COMPRESSED => {
            let enc_off = view.u16(page_off + 8)? as usize;
            let enc_count = view.u16(page_off + 10)? as usize;
            (
                view.u16(page_off + 4)? as usize,
                view.u16(page_off + 6)? as usize,
                encodings(view, page_off + enc_off, enc_count)?,
            )
        }
        _ => {
            return Err(Error::Custom(
                "unknown __unwind_info second-level page kind",
            ));
        }
    };
    if count > MAX_ENTRIES {
        return Err(Error::TooDeep(SECT));
    }

    for i in 0..count {
        let (function, encoding) = match kind {
            // The two kinds count from different places, and nothing in the
            // bytes says so: a regular entry's offset is from the image base,
            // a compressed one's from the page's own first function. Reading
            // either with the other's base produces addresses that look
            // plausible and are not in the right function.
            KIND_REGULAR => {
                let at = page_off + entries_off + i * 8;
                (base.wrapping_add(view.u32(at)? as Word), view.u32(at + 4)?)
            }
            _ => {
                let word = view.u32(page_off + entries_off + i * 4)?;
                // 24 bits of address, relative to this page's own base, and 8
                // bits selecting an encoding: the common array first, then the
                // page's own, which is what the index counts past.
                let function = page_base.wrapping_add((word & 0x00ff_ffff) as Word);
                let index = (word >> 24) as usize;
                let encoding = match commons.get(index) {
                    Some(&e) => e,
                    None => *page_encodings
                        .get(index - commons.len())
                        .ok_or(Error::Custom("__unwind_info encoding index out of range"))?,
                };
                (function, encoding)
            }
        };

        let personality = match (encoding & PERSONALITY_MASK) >> PERSONALITY_SHIFT {
            0 => None,
            // 1-based: index zero means "no personality", so entry *n* is
            // `personalities[n - 1]`. Reading it as 0-based silently attributes
            // every function to the wrong routine.
            n => Some(*personalities.get(n as usize - 1).ok_or(Error::Custom(
                "__unwind_info personality index out of range",
            ))?),
        };
        let lsda = (encoding & HAS_LSDA != 0)
            .then(|| lsdas.iter().find(|(f, _)| *f == function).map(|(_, l)| *l))
            .flatten();

        out.push(UnwindEntry {
            function: function.wrapping_add(slide),
            encoding,
            personality_slot: personality.map(|p| p.wrapping_add(slide)),
            lsda: lsda.map(|l| l.wrapping_add(slide)),
        });
    }
    Ok(())
}
