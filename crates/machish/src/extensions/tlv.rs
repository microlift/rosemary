use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::Result;
use crate::extensions::{Extension, read_words};
use crate::fixups::{Fixup, LibraryRef};
use crate::image::MachObject;
use crate::memory::AddressSpace;
use crate::raw::Word;
use crate::report::{ImageReport, ThreadLocalDescriptor, TlvTemplate};
use crate::spec::MachSpec;
use crate::spec::constants::POINTER_SIZE;

const DESCRIPTOR_WORDS: u64 = 3;
const DESCRIPTOR_SIZE: u64 = DESCRIPTOR_WORDS * 8;

pub const BOOTSTRAP_SYMBOLS: &[&str] = &["__tlv_bootstrap", "_tlv_bootstrap"];

#[derive(Clone, Debug)]
pub struct TlvExtension {
    /// Symbols whose bind is rewritten to a flat lookup.
    redirect: Vec<String>,
    /// Rename each redirected import to this, if set.
    rename_to: Option<String>,
}

impl Default for TlvExtension {
    fn default() -> Self {
        Self::new()
    }
}

impl TlvExtension {
    /// Redirect the standard bootstrap symbols.
    pub fn new() -> Self {
        TlvExtension {
            redirect: BOOTSTRAP_SYMBOLS.iter().map(|s| s.to_string()).collect(),
            rename_to: None,
        }
    }

    /// Enumerate descriptors but leave the fixup stream alone.
    pub fn observe_only() -> Self {
        TlvExtension {
            redirect: Vec::new(),
            rename_to: None,
        }
    }

    /// Also redirect `symbol`.
    pub fn redirect(&mut self, symbol: &str) -> &mut Self {
        self.redirect.push(symbol.to_string());
        self
    }

    pub fn rename_to(&mut self, name: &str) -> &mut Self {
        self.rename_to = Some(name.to_string());
        self
    }

    fn wants(&self, name: &str) -> bool {
        self.redirect.iter().any(|s| s == name)
    }
}

impl<S: MachSpec> Extension<S> for TlvExtension {
    fn name(&self) -> &'static str {
        "tlv"
    }

    fn claims(&self, obj: &MachObject<'_, S>) -> bool {
        obj.sections.iter().any(|s| s.is_thread_local())
    }

    fn transform_fixups<'a>(
        &self,
        _obj: &MachObject<'a, S>,
        fixups: &mut Vec<Fixup<'a>>,
    ) -> Result<()> {
        if self.redirect.is_empty() {
            return Ok(());
        }
        for f in fixups.iter_mut() {
            let Fixup::Bind { symbol, .. } = f else {
                continue;
            };
            if !self.wants(symbol.name) {
                continue;
            }
            symbol.library = LibraryRef::Flat;
            if let Some(new) = &self.rename_to {
                symbol.name = leak_static(new);
            }
        }
        Ok(())
    }

    fn observe(
        &self,
        obj: &MachObject<'_, S>,
        mem: &dyn AddressSpace,
        slide: Word,
        report: &mut ImageReport,
    ) -> Result<()> {
        report.thread_locals = collect_thread_locals(obj, mem, slide)?;
        report.tlv_template = tlv_template(obj, slide);
        report.tlv_init_funcs = collect_tlv_init_funcs(obj, mem, slide)?;
        Ok(())
    }
}

fn leak_static(name: &str) -> &'static str {
    use alloc::boxed::Box;
    use core::sync::atomic::{AtomicPtr, Ordering};

    // A tiny append-only list, because `no_std` has no `OnceLock` and this is
    // touched a few times per process.
    struct Node {
        name: &'static str,
        next: *mut Node,
    }
    static HEAD: AtomicPtr<Node> = AtomicPtr::new(core::ptr::null_mut());

    let mut cur = HEAD.load(Ordering::Acquire);
    while !cur.is_null() {
        // SAFETY: nodes are heap-allocated once, published with `Release`, and
        // never freed or mutated after publication, so the pointer is valid.
        let node = unsafe { &*cur };
        if node.name == name {
            return node.name;
        }
        cur = node.next;
    }

    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    let node = Box::into_raw(Box::new(Node {
        name: leaked,
        next: HEAD.load(Ordering::Acquire),
    }));
    // A lost race duplicates one small allocation and is otherwise harmless:
    // both copies are valid `'static` strs with equal contents.
    loop {
        // SAFETY: `node` is a live, uniquely-owned allocation being published.
        let head = HEAD.load(Ordering::Acquire);
        unsafe { (*node).next = head };
        if HEAD
            .compare_exchange(head, node, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return leaked;
        }
    }
}

/// How big one thread's block for this image must be, and where its initial
/// bytes come from.
///
/// `None` when the image has neither section, which is also when it has no
/// descriptors to index into one.
pub fn tlv_template<S: MachSpec>(obj: &MachObject<'_, S>, slide: Word) -> Option<TlvTemplate> {
    let data = obj.sections.iter().find(|s| s.is_thread_local_data());
    let bss = obj.sections.iter().find(|s| s.is_thread_local_bss());

    // Byte zero of the block is `__thread_data`'s first byte, or - for an image
    // whose thread-locals are all zero-initialised, which has no `__thread_data`
    // at all - `__thread_bss`'s.
    let start = data.or(bss)?.addr;
    let end = bss
        .or(data)
        .map(|s| s.addr.wrapping_add(s.size))
        .filter(|&e| e > start)?;

    Some(TlvTemplate {
        initial_va: start.wrapping_add(slide),
        // Only `__thread_data` has bytes in the file; everything past it is the
        // zero-filled tail, whether that is `__thread_bss` or the alignment
        // padding in front of it.
        initial_size: data.map_or(0, |s| s.size),
        total_size: end - start,
        // The stricter of the two sections' requirements, capped where an
        // allocation could not honour it anyway: `align` is a log2 in the file
        // and nothing checks it, so a corrupt image must not become a shift
        // overflow here.
        align: data
            .map_or(0, |s| s.align)
            .max(bss.map_or(0, |s| s.align))
            .min(16),
    })
}

pub fn collect_thread_locals<S: MachSpec>(
    obj: &MachObject<'_, S>,
    mem: &dyn AddressSpace,
    slide: Word,
) -> Result<Vec<ThreadLocalDescriptor>> {
    let mut out = Vec::new();
    for sect in obj
        .sections
        .iter()
        .filter(|s| s.is_thread_local_variables())
    {
        let count = usize::try_from(sect.entry_count(DESCRIPTOR_SIZE)).unwrap_or(0);
        let base = sect.addr.wrapping_add(slide);
        let words = read_words(mem, base, count * DESCRIPTOR_WORDS as usize)?;
        #[allow(clippy::chunks_exact_to_as_chunks)]
        for (i, chunk) in words.chunks_exact(DESCRIPTOR_WORDS as usize).enumerate() {
            out.push(ThreadLocalDescriptor {
                at: base.wrapping_add(i as u64 * DESCRIPTOR_SIZE),
                thunk: chunk[0],
                key: chunk[1],
                offset: chunk[2],
            });
        }
    }
    Ok(out)
}

/// Every `__thread_bss_init_func` entry, slid, in section order - the
/// functions that construct a thread's zero-initialised thread-locals.
///
/// Read from memory, where the rebase pass has already slid the stored
/// pointers, exactly as for `__mod_term_func`: these are addresses of
/// functions in the same image, not imports, so the slide is the whole of the
/// fixup. `slide` locates the section, not its contents.
///
/// The order is the section's, and it is *not* the order to call them in - see
/// [`ImageReport::tlv_init_funcs`], which is where the contract is written
/// down.
///
/// [`ImageReport::tlv_init_funcs`]: crate::report::ImageReport::tlv_init_funcs
pub fn collect_tlv_init_funcs<S: MachSpec>(
    obj: &MachObject<'_, S>,
    mem: &dyn AddressSpace,
    slide: Word,
) -> Result<Vec<Word>> {
    let mut out = Vec::new();
    for sect in obj
        .sections
        .iter()
        .filter(|s| s.is_thread_local_init_functions())
    {
        let count = usize::try_from(sect.entry_count(POINTER_SIZE)).unwrap_or(0);
        out.extend(read_words(mem, sect.addr.wrapping_add(slide), count)?);
    }
    // A null entry is padding, not something to call.
    out.retain(|&at| at != 0);
    Ok(out)
}
