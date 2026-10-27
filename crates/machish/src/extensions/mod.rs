#[cfg(feature = "ext-initializers")]
pub mod initializers;
#[cfg(feature = "ext-interpose")]
pub mod interpose;
#[cfg(feature = "ext-objc")]
pub mod objc;
#[cfg(feature = "ext-tlv")]
pub mod tlv;

use alloc::vec::Vec;

use crate::error::Result;
use crate::fixups::Fixup;
use crate::image::MachObject;
use crate::memory::AddressSpace;
use crate::raw::Word;
use crate::report::ImageReport;
use crate::spec::MachSpec;

pub trait Extension<S: MachSpec> {
    fn name(&self) -> &'static str;

    /// Whether this extension applies to `obj`.
    fn claims(&self, obj: &MachObject<'_, S>) -> bool {
        let _ = obj;
        true
    }

    /// Rewrite the decoded fixup stream before it is resolved and applied.
    fn transform_fixups<'a>(
        &self,
        obj: &MachObject<'a, S>,
        fixups: &mut Vec<Fixup<'a>>,
    ) -> Result<()> {
        let _ = (obj, fixups);
        Ok(())
    }

    /// Record what the image contains, reading through `mem` post-binding.
    fn observe(
        &self,
        obj: &MachObject<'_, S>,
        mem: &dyn AddressSpace,
        slide: Word,
        report: &mut ImageReport,
    ) -> Result<()> {
        let _ = (obj, mem, slide, report);
        Ok(())
    }
}

/// The extensions applied to every image in a link, in order.
pub struct ExtensionSet<S: MachSpec> {
    extensions: Vec<alloc::boxed::Box<dyn Extension<S>>>,
}

impl<S: MachSpec> Default for ExtensionSet<S> {
    fn default() -> Self {
        ExtensionSet {
            extensions: Vec::new(),
        }
    }
}

impl<S: MachSpec> ExtensionSet<S> {
    /// An empty set: nothing is collected, nothing is transformed.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The extensions enabled by this build's features.
    ///
    /// Everything here only reports, so enabling them by default cannot
    /// change what an image does, it can only make the [`LinkReport`] say more.
    ///
    /// [`LinkReport`]: crate::report::LinkReport
    #[allow(unused_mut, clippy::vec_init_then_push)]
    pub fn defaults() -> Self {
        let mut extensions: Vec<alloc::boxed::Box<dyn Extension<S>>> = Vec::new();
        #[cfg(feature = "ext-initializers")]
        extensions.push(alloc::boxed::Box::new(initializers::InitializersExtension));
        #[cfg(feature = "ext-tlv")]
        extensions.push(alloc::boxed::Box::new(tlv::TlvExtension::new()));
        #[cfg(feature = "ext-interpose")]
        extensions.push(alloc::boxed::Box::new(interpose::InterposeExtension));
        #[cfg(feature = "ext-objc")]
        extensions.push(alloc::boxed::Box::new(objc::ObjcExtension));
        ExtensionSet { extensions }
    }

    pub fn push(&mut self, ext: alloc::boxed::Box<dyn Extension<S>>) -> &mut Self {
        self.extensions.push(ext);
        self
    }

    pub fn len(&self) -> usize {
        self.extensions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.extensions.iter().map(|e| e.name())
    }

    pub(crate) fn transform_fixups<'a>(
        &self,
        obj: &MachObject<'a, S>,
        fixups: &mut Vec<Fixup<'a>>,
    ) -> Result<()> {
        for e in &self.extensions {
            if e.claims(obj) {
                e.transform_fixups(obj, fixups)?;
            }
        }
        Ok(())
    }

    pub(crate) fn observe(
        &self,
        obj: &MachObject<'_, S>,
        mem: &dyn AddressSpace,
        slide: Word,
        report: &mut ImageReport,
    ) -> Result<()> {
        for e in &self.extensions {
            if e.claims(obj) {
                e.observe(obj, mem, slide, report)?;
            }
        }
        Ok(())
    }
}

#[cfg(any(
    feature = "ext-initializers",
    feature = "ext-tlv",
    feature = "ext-interpose"
))]
pub(crate) fn read_words(mem: &dyn AddressSpace, at: Word, count: usize) -> Result<Vec<Word>> {
    let mut out = Vec::with_capacity(count.min(1024));
    for i in 0..count {
        let mut buf = [0u8; 8];
        mem.read(at.wrapping_add(i as u64 * 8), &mut buf)?;
        out.push(u64::from_le_bytes(buf));
    }
    Ok(out)
}
