use alloc::vec::Vec;
use core::ops::Range;

use crate::error::{Error, Result};
use crate::image::MachObject;
use crate::memory::{
    AddressSpace, FileBacking, Placement, Prot, ProtectionPolicy, align_down, align_up,
};
use crate::raw::{View, Word};
use crate::spec::MachSpec;
use crate::spec::constants::SEGMENT_COMMAND_64_SIZE;

/// One `LC_SEGMENT_64` record, with the range of [`Section`]s it owns.
///
/// [`Section`]: crate::image::Section
#[derive(Clone, Debug)]
pub struct Segment<'a> {
    pub name: &'a str,
    /// Virtual address, before any slide.
    pub vmaddr: Word,
    pub vmsize: Word,
    pub fileoff: u64,
    pub filesize: u64,
    /// The strongest protection this segment may ever hold.
    pub maxprot: i32,
    /// The protection it should hold once loaded.
    pub initprot: i32,
    pub flags: u32,
    /// Indices into [`MachObject::sections`](crate::image::MachObject::sections).
    pub sections: Range<usize>,
}

impl<'a> Segment<'a> {
    /// Parse the fixed part of a `segment_command_64` at `off`. The caller
    /// parses the trailing sections and fills in [`Segment::sections`].
    pub fn parse(view: &View<'a>, off: usize) -> Result<(Self, u32)> {
        let nsects = view.u32(off + 64)?;
        let seg = Segment {
            name: view.fixed_str(off + 8, 16)?,
            vmaddr: view.u64(off + 24)?,
            vmsize: view.u64(off + 32)?,
            fileoff: view.u64(off + 40)?,
            filesize: view.u64(off + 48)?,
            maxprot: view.i32(off + 56)?,
            initprot: view.i32(off + 60)?,
            flags: view.u32(off + 68)?,
            sections: 0..0,
        };
        Ok((seg, nsects))
    }

    /// Offset of the first `section_64` following this segment command.
    pub const fn sections_offset(cmd_off: usize) -> usize {
        cmd_off + SEGMENT_COMMAND_64_SIZE
    }

    /// Whether this is the null guard rather than real content.
    ///
    /// `__PAGEZERO` is a 4 GiB unreadable, unwritable, file-backed-by-nothing
    /// reservation covering `[0, 0x1_0000_0000)`, whose only job is to make null
    /// and small-integer dereferences fault. It must never be mapped: including
    /// it in the span computation would ask the address space to reserve four
    /// gigabytes and would put the image's base at zero.
    ///
    /// A convenient accident makes skipping it free rather than lossy. On arm64
    /// macOS the host process's own `__PAGEZERO` already covers exactly that
    /// range, so a guest null dereference faults for the same reason it would
    /// have natively, with nothing mapped on our part. Do not "fix" this by
    /// mapping the segment.
    ///
    /// Detected by protection and file extent rather than by name, since the
    /// name is convention and a hostile or unusual linker may differ.
    pub fn is_page_zero(&self) -> bool {
        (self.initprot == 0 && self.maxprot == 0 && self.filesize == 0) || self.name == "__PAGEZERO"
    }

    /// Whether the loader should reserve address space for this segment.
    pub fn is_mappable(&self) -> bool {
        !self.is_page_zero() && self.vmsize > 0
    }

    /// The protection this segment wants once loaded.
    pub fn prot(&self) -> Prot {
        Prot::from_vm_prot(self.initprot)
    }

    /// End of the segment's virtual extent.
    pub fn vmend(&self) -> Word {
        self.vmaddr.wrapping_add(self.vmsize)
    }

    /// Whether `addr` falls inside this segment.
    pub fn contains(&self, addr: Word) -> bool {
        addr >= self.vmaddr && addr < self.vmend()
    }

    /// The file range backing this segment, if any.
    pub fn file_range(&self) -> Option<Range<usize>> {
        if self.filesize == 0 {
            return None;
        }
        let start = usize::try_from(self.fileoff).ok()?;
        let len = usize::try_from(self.filesize).ok()?;
        Some(start..start.checked_add(len)?)
    }
}

/// The result of mapping an image.
#[derive(Copy, Clone, Debug)]
pub struct MappedImage {
    /// Address the image actually landed at.
    pub base: Word,
    /// Added to every address in the file to get its mapped address.
    /// Zero when the image landed on its preferred base, which is the
    /// case the recompiler's identity mapping wants.
    pub slide: Word,
    /// Total bytes reserved, across every run of contiguous segments.
    pub span: usize,
    /// Absolute entry point, if the image declares one.
    pub entry: Option<Word>,
    /// The entry-point command as declared, kept alongside the resolved address
    /// because the two encodings do not agree on how the process arguments
    /// reach the entry point: an `LC_MAIN` entry is a C function taking them in
    /// registers, while an `LC_UNIXTHREAD` entry expects the kernel's frame on
    /// the stack. Resolving to an address alone loses that.
    pub entry_command: Option<crate::image::EntryPoint>,
}

/// A host page whose protection had to be widened because several segments with
/// differing permissions share it.
///
/// Reported rather than swallowed: this is the visible consequence of an
/// x86_64 image's 4 KiB layout meeting a 16 KiB host, and silently
/// over-permissive pages are exactly the kind of thing that should be
/// inspectable rather than discovered later.
#[derive(Clone, Debug)]
pub struct PageConflict<'a> {
    pub page: Word,
    pub applied: Prot,
    pub segments: Vec<&'a str>,
}

/// Reserve space and copy every mappable segment in.
///
/// Segments are left writable so fixups can be applied; call
/// [`protect_segments`] afterwards.
///
/// Placement follows the image: a PIE tolerates a slide and asks for
/// [`Placement::Preferred`], while a non-PIE image has absolute addresses baked
/// into its code and must use [`Placement::Fixed`] - a silent slide there would
/// produce an image that links cleanly and jumps into nothing.
pub fn map_segments<S: MachSpec, M: AddressSpace>(
    obj: &MachObject<'_, S>,
    mem: &mut M,
) -> Result<MappedImage> {
    map_segments_from(obj, mem, None)
}

/// [`map_segments`], letting `mem` map each segment from `file` rather than
/// copy it, where the space supports that and the file still holds the bytes.
pub fn map_segments_from<S: MachSpec, M: AddressSpace>(
    obj: &MachObject<'_, S>,
    mem: &mut M,
    file: Option<FileBacking<'_>>,
) -> Result<MappedImage> {
    let page = obj.spec().page_size();
    let preferred = obj.preferred_base()?;

    // Reserve to the coarser of the image's own page size and the host's
    // protection granularity. An x86_64 image is laid out at 4 KiB while Apple
    // Silicon protects at 16 KiB, so the protection sweep rounds segment ends
    // up to 16 KiB - and would then run past a reservation sized only to 4 KiB.
    let gran = page.max(mem.protect_granularity()).max(1);
    let runs = reservation_runs(obj, gran);
    if runs.is_empty() {
        return Err(Error::NoLoadableSegments);
    }

    // The run holding the header decides the slide; the others follow it, as
    // xnu slides every segment of an image by the same amount.
    let first = runs
        .iter()
        .position(|r| r.contains(&preferred))
        .unwrap_or(0);
    let mut span = 0;
    let mut slide: Word = 0;
    for i in core::iter::once(first).chain((0..runs.len()).filter(|&i| i != first)) {
        let run = &runs[i];
        let len = usize::try_from(run.end - run.start)
            .map_err(|_| Error::Memory("segment run too large"))?;
        let placement = if i != first {
            Placement::Fixed(run.start.wrapping_add(slide))
        } else if obj.is_pie() {
            Placement::Preferred(run.start)
        } else {
            Placement::Fixed(run.start)
        };
        let got = mem.reserve(len, gran as usize, placement)?;
        if i == first {
            slide = got.wrapping_sub(run.start);
        }
        span += len;
    }
    let base = preferred.wrapping_add(slide);

    let bytes = obj.bytes();
    for seg in obj.mappable_segments() {
        let Some(range) = seg.file_range() else {
            continue;
        };
        // A segment may legitimately declare more virtual than file size - the
        // remainder is zero-fill, already zero from `reserve`. It may not
        // declare more *file* size than the file has.
        let src = bytes.get(range.clone()).ok_or(Error::Truncated {
            what: "segment contents",
        })?;
        let addr = seg.vmaddr.wrapping_add(slide);
        let mapped = match file {
            Some(f) => mem.map_file(addr, f.path, f.offset + range.start as u64, src)?,
            None => 0,
        };
        if mapped < src.len() {
            mem.write(addr + mapped as Word, &src[mapped..])?;
        }
    }

    Ok(MappedImage {
        base,
        slide,
        span,
        entry: obj.entry().map(|e| e.address(base, slide)),
        entry_command: obj.entry(),
    })
}

/// The address ranges to reserve: mappable segments rounded out to `gran`,
/// merged where they touch, in address order.
///
/// One range per run rather than one spanning the image, because the gaps
/// between segments are not the image's. Wine's loader puts zero-fill segments
/// at `0x1000` and `0x7ff0_0000_0000` around a header at `0x2_0000_0000`, and a
/// single reservation would claim the whole 128 TiB between them.
fn reservation_runs<S: MachSpec>(obj: &MachObject<'_, S>, gran: Word) -> Vec<Range<Word>> {
    let mut segs: Vec<Range<Word>> = obj
        .mappable_segments()
        .map(|s| align_down(s.vmaddr, gran)..align_up(s.vmend(), gran))
        .collect();
    segs.sort_unstable_by_key(|r| r.start);
    let mut runs: Vec<Range<Word>> = Vec::with_capacity(segs.len());
    for seg in segs {
        match runs.last_mut() {
            Some(last) if seg.start <= last.end => last.end = last.end.max(seg.end),
            _ => runs.push(seg),
        }
    }
    runs
}

/// Apply each segment's `initprot`, reconciled against the host's protection
/// granularity per `policy`.
///
/// The `exec` bit is dropped unless the spec says the host can execute this
/// image. For an x86-64 guest on Apple Silicon it never can, so asking for
/// `PROT_EXEC` would request a permission that means nothing and, on macOS,
/// interacts badly with hardened runtime policy.
pub fn protect_segments<'a, S: MachSpec, M: AddressSpace>(
    obj: &MachObject<'a, S>,
    mem: &mut M,
    slide: Word,
    policy: ProtectionPolicy,
) -> Result<Vec<PageConflict<'a>>> {
    if policy == ProtectionPolicy::None {
        return Ok(Vec::new());
    }

    let executable = obj.spec().natively_executable();
    let gran = mem.protect_granularity().max(1);
    let segs: Vec<_> = obj.mappable_segments().collect();

    if policy == ProtectionPolicy::Strict {
        for seg in &segs {
            if align_down(seg.vmaddr, gran) != seg.vmaddr
                || align_up(seg.vmend(), gran) != seg.vmend()
            {
                return Err(Error::Memory("segment not aligned to host page size"));
            }
        }
        for seg in &segs {
            let prot = if executable {
                seg.prot()
            } else {
                seg.prot().without_exec()
            };
            let len =
                usize::try_from(seg.vmsize).map_err(|_| Error::Memory("segment too large"))?;
            mem.protect(seg.vmaddr.wrapping_add(slide), len, prot)?;
        }
        return Ok(Vec::new());
    }

    // Coalesce. Sweep the page-aligned boundaries of every segment and, for
    // each elementary interval between consecutive boundaries, take the union
    // of the permissions of every segment covering it. Union is the only safe
    // direction: it never removes a permission something needs.
    let mut bounds: Vec<Word> = Vec::with_capacity(segs.len() * 2);
    for seg in &segs {
        bounds.push(align_down(seg.vmaddr, gran));
        bounds.push(align_up(seg.vmend(), gran));
    }
    bounds.sort_unstable();
    bounds.dedup();

    let mut conflicts = Vec::new();
    // (start, end, prot) runs, merged when adjacent and equal.
    let mut runs: Vec<(Word, Word, Prot)> = Vec::new();

    for w in bounds.windows(2) {
        let (start, end) = (w[0], w[1]);
        let mut prot = Prot::NONE;
        let mut covering: Vec<&'a str> = Vec::new();
        for seg in &segs {
            let s = align_down(seg.vmaddr, gran);
            let e = align_up(seg.vmend(), gran);
            if s < end && start < e {
                prot = prot.union(seg.prot());
                covering.push(seg.name);
            }
        }
        if covering.is_empty() {
            continue;
        }
        // Strip exec before recording the conflict, so `applied` is literally
        // the protection that gets applied rather than an intermediate value.
        if !executable {
            prot = prot.without_exec();
        }
        if covering.len() > 1 {
            // More than one segment in this page: the permissions we apply are
            // necessarily weaker than any single segment asked for.
            conflicts.push(PageConflict {
                page: start,
                applied: prot,
                segments: covering,
            });
        }
        match runs.last_mut() {
            Some(last) if last.1 == start && last.2 == prot => last.1 = end,
            _ => runs.push((start, end, prot)),
        }
    }

    for (start, end, prot) in runs {
        let len = usize::try_from(end - start).map_err(|_| Error::Memory("range too large"))?;
        mem.protect(start.wrapping_add(slide), len, prot)?;
    }
    Ok(conflicts)
}
