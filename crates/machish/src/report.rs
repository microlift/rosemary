use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::fixups::BindKind;
use crate::raw::Word;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingRecord {
    /// Slid VA of the pointer slot that was written. Pass this to
    /// `register_fast_import` to match a `call [rip+disp]` site, which is what
    /// the recompiler compares against `memory_displacement64()`.
    pub slot_va: Word,
    /// Slid VA of the `__stubs` entry that jumps through `slot_va`, when the
    /// image has one. Pass this to match a `call rel32` site, which is the
    /// shape of essentially every import call in a classic image.
    pub stub_va: Option<Word>,
    /// Install name the import asked for, absent for a flat or special
    /// reference.
    pub library: Option<String>,
    /// Symbol name, leading underscore preserved.
    pub symbol: String,
    /// The address written into the slot. Zero for a weak import that no
    /// provider satisfied.
    pub resolved: Word,
    pub kind: BindKind,
    pub weak_import: bool,
    /// Verbatim echo of [`crate::resolve::Resolved::tag`].
    pub tag: u64,
}

impl BindingRecord {
    /// Whether some provider actually answered.
    pub fn is_satisfied(&self) -> bool {
        self.resolved != 0
    }
}

/// An import nothing could satisfy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingSymbol {
    pub symbol: String,
    pub library: Option<String>,
    /// Slid VA of the slot left unwritten.
    pub slot_va: Word,
    /// Slid VA of the `__stubs` entry fronting it, when there is one.
    ///
    /// Reported for the same reason [`BindingRecord::stub_va`] is, and it is the
    /// unresolved case that needs it most. A recompiler told only about slots
    /// follows `call rel32` into the six-byte stub, decodes `jmp qword
    /// [rip+disp]` as guest code, and lands on the zero the binder wrote - so the
    /// diagnostic a program gets for an import nobody satisfied is "no code at
    /// 0x0" rather than the symbol's name. See `rosetta3::Program`.
    pub stub_va: Option<Word>,
    /// Whether the image tolerates the absence.
    pub weak_import: bool,
}

/// One `{ replacement, replacee }` pair from a `__DATA,__interpose` section,
/// as both halves stand in memory once the image is bound.
///
/// The request it states is dyld's: *every* binding in every other image that
/// resolved to `replacee` should resolve to `replacement` instead. Both are
/// addresses rather than names on purpose - that is what makes an interposition
/// catch a call the interposing image could not have named, such as one made
/// through an alias.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Interposition {
    /// Where calls should go: a function in the interposing image.
    pub replacement: Word,
    /// What is being replaced: whatever the interposer's own import of that
    /// symbol resolved to, which is the address every other image's import of
    /// it also resolved to.
    pub replacee: Word,
}

/// Everything binding one image produced.
#[derive(Clone, Debug, Default)]
pub struct ImageReport {
    /// The image's install name, or its path, for diagnostics.
    pub name: String,
    pub base: Word,
    pub slide: Word,
    /// Which fixup encoding the image used, from
    /// [`crate::fixups::FixupDecoder::name`].
    pub encoding: &'static str,
    pub bindings: Vec<BindingRecord>,
    /// Imports left unsatisfied. Non-weak entries here mean the image will
    /// fault the first time it uses one.
    pub missing: Vec<MissingSymbol>,
    /// How many pointers were slid.
    pub rebases: usize,
    /// Slid VAs of `S_MOD_INIT_FUNC_POINTERS` entries, in section order.
    /// Collected, never called - running them means executing guest code, which
    /// is the driver's job.
    pub initializers: Vec<Word>,
    /// Slid VAs of `S_MOD_TERM_FUNC_POINTERS` entries. Reported, never run.
    pub terminators: Vec<Word>,
    /// What the image's `__DATA,__interpose` section asks for, decoded and not
    /// acted on. Empty for the overwhelming majority of images, which declare no
    /// such section at all.
    pub interpositions: Vec<Interposition>,
    /// Thread-local descriptors, enumerated and not otherwise honoured.
    pub thread_locals: Vec<ThreadLocalDescriptor>,
    /// Slid addresses of the image's `__thread_bss_init_func` entries, in
    /// section order.
    ///
    /// The one part of the thread-local contract that cannot be recovered from
    /// the descriptors, and the reason this is a field of its own rather than
    /// part of [`TlvTemplate`]: a template is where a block's bytes come from,
    /// and this is a list of functions to *call* once a thread first has one.
    ///
    /// What a caller owes them, from `tlv_allocate_and_initialize_for_key` in
    /// dyld-852.2 (`src/threadLocalVariables.c`) - the last Apple code that ran
    /// this section at all:
    ///
    /// * the type is `void (*)(void)` - **not** a function of the block, which
    ///   `tlv_get_addr` itself has already turned into an address;
    /// * they run on the first use of *any* thread-local in this image on a
    ///   given thread, not on the first use of the variable each belongs to;
    /// * they run **back to front** - `for (size_t j=count; j > 0; --j)`, so a
    ///   later-registered constructor is torn down first;
    /// * the block is already in the thread's storage when they run, because
    ///   dyld does `pthread_setspecific(key, buffer)` *before* this pass. An
    ///   initialiser that reads another thread-local of the same image
    ///   therefore finds it constructed rather than re-entering the slow path.
    ///
    /// Expect this to be empty in anything built with a current toolchain: clang
    /// stopped emitting `__thread_bss_init_func` years ago and constructs a
    /// `thread_local` with a dynamic initialiser inline, under a guard byte in
    /// the variable, at the access. `Chromium Framework` 130.0.6723.91 has 116
    /// descriptors and no such section, which is why a link that honoured only
    /// this list would be complete for that image by having nothing to do.
    ///
    /// Collected, never called: running them means executing guest code, which
    /// is the driver's job, exactly as for [`initializers`](Self::initializers).
    pub tlv_init_funcs: Vec<Word>,
    /// The image's thread-local block layout, if it has one. Present exactly
    /// when [`thread_locals`](ImageReport::thread_locals) is non-empty and the
    /// image carries a `__thread_data` or `__thread_bss` section.
    pub tlv_template: Option<TlvTemplate>,
    /// What the image declares to the Objective-C runtime, decoded and
    /// otherwise untouched: no class is realized, no selector uniqued, no
    /// category attached. `None` when the image carries no `__objc_*` sections
    /// - an image that declares nothing is distinct from one not looked at.
    ///
    /// Gated on the decoder's own feature: without it there is nothing that
    /// could fill this in.
    #[cfg(feature = "ext-objc")]
    pub objc: Option<crate::extensions::objc::ObjcImage>,
    /// Where each segment landed, with the file bytes backing it, so a driver
    /// can hand exact ranges to a recompiler without re-reading memory.
    pub segments: Vec<SegmentReport>,
    /// Entry point, if the image declares one.
    pub entry: Option<Word>,
    /// Which of the two entry-point commands declared it. Kept because the
    /// address alone does not say how the process arguments are meant to
    /// arrive: see [`MappedImage::entry_command`](crate::image::MappedImage::entry_command).
    pub entry_command: Option<crate::image::EntryPoint>,
}

impl ImageReport {
    /// Imports that are missing *and* not weak.
    pub fn hard_missing(&self) -> impl Iterator<Item = &MissingSymbol> {
        self.missing.iter().filter(|m| !m.weak_import)
    }

    /// `Err` if any non-weak import went unresolved.
    ///
    /// Separate from binding itself so the caller sees the *whole* list rather
    /// than whichever symbol happened to be decoded first - with a shim table
    /// under construction, "these forty symbols are missing" is the useful
    /// message and "`_strlen` is missing" is forty round trips.
    pub fn require_all_resolved(&self) -> Result<()> {
        let mut it = self.hard_missing();
        let Some(first) = it.next() else {
            return Ok(());
        };
        let rest = it.count();
        let mut msg = first.symbol.clone();
        if rest > 0 {
            msg.push_str(" (and ");
            push_usize(&mut msg, rest);
            msg.push_str(" more)");
        }
        Err(Error::UnresolvedSymbol(msg))
    }
}

/// The layout of one image's per-thread block.
///
/// A `tlv_descriptor`'s `offset` is a byte offset into a block the runtime
/// allocates per thread, and the block is the image's `__thread_data` followed
/// by its `__thread_bss` - which is why an offset alone is not enough to honour
/// one. This is the rest: where the initial values live and how big the whole
/// block has to be.
///
/// Both halves are optional in an image and neither is optional in the
/// arithmetic, so the fields are absolute: `total_size` spans from the start of
/// `__thread_data` (or of `__thread_bss`, if there is no data) to the end of
/// `__thread_bss`, alignment padding included. Anything past `initial_size` is
/// zero, which is what makes "copy the template, zero the rest" correct without
/// consulting the section table again.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TlvTemplate {
    /// Slid VA the block's byte zero is copied from.
    pub initial_va: Word,
    /// How many bytes of the block have initial values.
    pub initial_size: Word,
    /// The whole block's size, including the zero-filled tail.
    pub total_size: Word,
    /// Byte alignment the block must be allocated at, as `1 << align`.
    ///
    /// Reported because a thread-local may be over-aligned - `_Thread_local
    /// __attribute__((aligned(64)))` is legal - and an allocator's default
    /// alignment would satisfy the common case and silently not the declared
    /// one.
    pub align: u32,
}

/// One thread-local variable's `tlv_descriptor`, as found in
/// `S_THREAD_LOCAL_VARIABLES`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ThreadLocalDescriptor {
    /// Slid VA of the descriptor itself.
    pub at: Word,
    /// The thunk a guest calls to get the variable's per-thread address. In an
    /// unbound image this is a `__tlv_bootstrap` import.
    ///
    /// The call is `lea rdi, [desc]; call *desc.thunk`, and what comes back is
    /// **the variable's own address**, not the block's: `_tlv_get_addr` ends
    /// with `addq 16(%rdi),%rax` (dyld-852.2 `src/threadLocalHelpers.s`), and
    /// the code clang emits for a `thread_local` then indexes the variable at
    /// offset zero. `offset` is added by the thunk, which is why a descriptor's
    /// `offset` is invisible to the instruction that follows the call.
    ///
    /// The thunk also has a calling convention of its own: dyld's own comment
    /// on it is "returns address of TLV in %rax, all other registers
    /// preserved", and the reason is that computing a thread-local's address is
    /// emitted without spilling anything first. Anything bound here has to keep
    /// every register but the result, including `rdi`-`r11` and `xmm0`-`xmm7`.
    pub thunk: Word,
    /// The pthread key, zero until the runtime allocates one. We never do.
    pub key: Word,
    /// Byte offset of this variable within the thread's block.
    ///
    /// Relative to [`TlvTemplate::initial_va`], which is where the block's byte
    /// zero is: `__thread_data` if the image has one, `__thread_bss` if not.
    pub offset: Word,
}

/// Where a segment landed and which file bytes back it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentReport {
    pub name: String,
    /// Slid VA of the segment start.
    pub vmaddr: Word,
    pub vmsize: Word,
    /// Byte range within the (thin) image backing it, absent for zero-fill.
    pub file_range: Option<core::ops::Range<usize>>,
    /// Whether the segment holds code the recompiler should lift.
    pub executable: bool,
    /// Whether the guest may store to the segment. A recompiler must not fold a
    /// load from one of these to a constant: the bytes it reads at lift time are
    /// not the bytes that will be there at run time.
    pub writable: bool,
}

/// Every image linked together, in load order.
#[derive(Clone, Debug, Default)]
pub struct LinkReport {
    pub images: Vec<ImageReport>,
}

impl LinkReport {
    /// The first image, which by construction is the one the caller asked for.
    pub fn root(&self) -> Option<&ImageReport> {
        self.images.first()
    }

    /// Every binding across every image.
    pub fn bindings(&self) -> impl Iterator<Item = &BindingRecord> {
        self.images.iter().flat_map(|i| i.bindings.iter())
    }

    /// `Err` naming a missing symbol if any image has an unsatisfied hard
    /// import.
    pub fn require_all_resolved(&self) -> Result<()> {
        for img in &self.images {
            img.require_all_resolved()?;
        }
        Ok(())
    }
}

/// Append a decimal `usize`. `core::fmt` would do this, but pulling a
/// `Formatter` in for one number is more machinery than the job needs.
fn push_usize(s: &mut String, mut v: usize) {
    if v == 0 {
        s.push('0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    for &b in &buf[i..] {
        s.push(b as char);
    }
}
