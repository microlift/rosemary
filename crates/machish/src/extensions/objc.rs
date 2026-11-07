use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::image::MachObject;
use crate::memory::AddressSpace;
use crate::raw::Word;
use crate::report::ImageReport;
use crate::spec::MachSpec;

use super::Extension;

/// Section names. Matched by section name across every segment, never by
/// `(segment, section)`: the metadata moved from `__DATA` to `__DATA_CONST`
/// when the linker started emitting a read-only half, and an image built by
/// either toolchain is one we have to read.
mod sect {
    pub const IMAGE_INFO: &str = "__objc_imageinfo";
    pub const CLASSLIST: &str = "__objc_classlist";
    pub const NLCLSLIST: &str = "__objc_nlclslist";
    pub const CATLIST: &str = "__objc_catlist";
    pub const NLCATLIST: &str = "__objc_nlcatlist";
    pub const PROTOLIST: &str = "__objc_protolist";
    pub const SELREFS: &str = "__objc_selrefs";
    pub const MSGREFS: &str = "__objc_msgrefs";
    pub const CLASSREFS: &str = "__objc_classrefs";
    pub const SUPERREFS: &str = "__objc_superrefs";
    pub const PROTOREFS: &str = "__objc_protorefs";
}

/// `struct objc_class`: isa, superclass, two cache words, bits.
const CLASS_SIZE: u64 = 40;
/// Offset of `bits` within `objc_class`.
const CLASS_BITS_OFF: u64 = 32;
/// `struct class_ro_t`.
const CLASS_RO_SIZE: u64 = 72;
/// `struct method_t`.
const METHOD_SIZE: u64 = 24;
/// `struct ivar_t`.
const IVAR_SIZE: u64 = 32;
/// `struct property_t`.
const PROPERTY_SIZE: u64 = 16;

/// Mask isolating the `class_ro_t` pointer inside `class_data_bits_t`.
///
/// The low three bits are the runtime's own flags - two Swift markers and
/// "has default retain/release" - set on a class the compiler could describe
/// that way. They are not part of the address.
const FAST_DATA_MASK: Word = 0x0000_7fff_ffff_fff8;

/// `bits` low-bit markers. A class with either set is Swift: the header is the
/// start of Swift's class metadata, and `bits` still addresses a `class_ro_t`.
const FAST_IS_SWIFT_LEGACY: Word = 1 << 0;
const FAST_IS_SWIFT_STABLE: Word = 1 << 1;

/// Set in a method list's entry-size word when entries are relative.
const METHOD_LIST_IS_SMALL: u32 = 0x8000_0000;

/// `struct method_t::small`: three self-relative `int32`s.
const SMALL_METHOD_SIZE: u64 = 12;

/// The entry-size field's low bits are flags in every list header.
const ENTSIZE_MASK: u32 = 0x0000_fffc;

/// `class_ro_t::flags` bit marking a metaclass.
pub const RO_META: u32 = 1 << 0;
/// `class_ro_t::flags` bit marking a root class.
pub const RO_ROOT: u32 = 1 << 1;
/// `class_ro_t::flags` bit marking a Swift class whose metadata Swift must
/// finish at run time, through an initializer stored after the `class_ro_t`.
pub const RO_HAS_SWIFT_INITIALIZER: u32 = 1 << 6;

/// Image-info flag saying `category_t` carries a class-properties field.
///
/// A category has no size word, so this is the only thing that says whether its
/// seventh pointer exists. Reading it unconditionally walks off the end of the
/// last category in an image built before the field did.
pub const IMAGE_INFO_HAS_CATEGORY_CLASS_PROPERTIES: u32 = 1 << 6;

/// Ceiling on any single list's entry count, against a corrupt header claiming
/// a class has four billion methods.
const MAX_ENTRIES: usize = 1 << 20;

/// Ceiling on a NUL-terminated name.
const MAX_NAME: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ObjcRefusal {
    /// A list in the relative encoding that is not a method list. Only method
    /// lists have the small form; an ivar or property list claiming it is a
    /// header this module has read wrongly or an image that is malformed, and
    /// either way its entries cannot be twelve bytes of offsets.
    SmallMethodList { at: Word },
    /// A pointer that should have addressed this image's own metadata and did
    /// not. Reported with both ends so the graph edge is identifiable.
    ForeignPointer { from: Word, to: Word },
    /// A structure whose declared size disagrees with the one this decodes.
    UnexpectedSize {
        at: Word,
        declared: u32,
        expected: u32,
    },
}

/// `__objc_imageinfo`: two words the compiler writes about the whole image.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ObjcImageInfo {
    pub version: u32,
    pub flags: u32,
}

impl ObjcImageInfo {
    /// Whether categories in this image carry a class-properties pointer.
    pub fn has_category_class_properties(&self) -> bool {
        self.flags & IMAGE_INFO_HAS_CATEGORY_CLASS_PROPERTIES != 0
    }
}

/// One entry of a `method_list_t`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjcMethod {
    /// The selector, as the string it still is before uniquing.
    pub name: String,
    /// The type encoding, e.g. `i20@0:8i16`. This is the only description of a
    /// method's signature that exists anywhere, and it is what makes a call
    /// into one marshallable.
    pub types: String,
    /// The implementation's virtual address, in the guest.
    pub imp: Word,
}

/// One entry of an `ivar_list_t`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjcIvar {
    /// Address of the `int32_t` the compiler emitted for this ivar's offset.
    /// The runtime rewrites it during realization, which is why it is a pointer
    /// and not a value.
    pub offset_va: Word,
    /// The offset currently stored there.
    pub offset: u32,
    pub name: String,
    pub types: String,
    /// Alignment as a power of two; `!0` means "pointer-aligned".
    pub alignment: u32,
    pub size: u32,
}

/// One entry of a `property_list_t`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjcProperty {
    pub name: String,
    /// The attribute string, e.g. `Ti,N,V_n`.
    pub attributes: String,
}

/// A `class_ro_t`: the half of a class the compiler emits and the runtime
/// treats as read-only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassRo {
    pub va: Word,
    pub flags: u32,
    /// Where this class's own ivars begin, as the compiler laid them out.
    pub instance_start: u32,
    pub instance_size: u32,
    pub name: String,
    pub methods: Vec<ObjcMethod>,
    pub ivars: Vec<ObjcIvar>,
    pub properties: Vec<ObjcProperty>,
    /// Addresses of the `protocol_t`s this class conforms to.
    pub protocols: Vec<Word>,
    /// The function Swift finishes this class's metadata with, stored after
    /// the `class_ro_t` when [`RO_HAS_SWIFT_INITIALIZER`] is set; zero otherwise.
    pub swift_initializer: Word,
}

impl ClassRo {
    pub fn is_meta(&self) -> bool {
        self.flags & RO_META != 0
    }
    pub fn is_root(&self) -> bool {
        self.flags & RO_ROOT != 0
    }
    pub fn has_swift_initializer(&self) -> bool {
        self.flags & RO_HAS_SWIFT_INITIALIZER != 0
    }
}

/// A class as `__objc_classlist` points at it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjcClass {
    /// Address of the `objc_class`. This is the value `_OBJC_CLASS_$_Name`
    /// names, so it is what another image's bind resolves to.
    pub va: Word,
    /// The metaclass's address.
    pub isa: Word,
    /// The superclass's address - bound so for a class deriving from
    /// `NSObject` this is an address in the host's `libobjc`, not in the guest.
    pub superclass: Word,
    /// The raw `class_data_bits_t`, before masking. Reported as well as
    /// followed because the three low bits are the only thing that says a class
    /// is Swift, and [`ro`](ObjcClass::ro) has them masked off by construction.
    pub bits: Word,
    pub ro: ClassRo,
    /// The metaclass, decoded when it lies in this image. Its `ro` holds the
    /// class methods; a class's `+` methods live nowhere else.
    pub meta: Option<ObjcMetaclass>,
}

impl ObjcClass {
    /// Whether `bits` carries one of the two Swift markers.
    ///
    /// Such a class's [`ro`](ObjcClass::ro) is real, but Swift's own metadata
    /// follows the header, so a consumer must keep the class where it is
    /// rather than copy it.
    pub fn is_swift(&self) -> bool {
        self.bits & (FAST_IS_SWIFT_LEGACY | FAST_IS_SWIFT_STABLE) != 0
    }
}

/// A metaclass. Structurally an [`ObjcClass`] whose own `isa` is the root
/// metaclass, and separate so the type says the recursion stops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjcMetaclass {
    pub va: Word,
    pub isa: Word,
    pub superclass: Word,
    pub ro: ClassRo,
}

/// A `category_t` as `__objc_catlist` points at it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjcCategory {
    pub va: Word,
    /// The category's own name - `Extra` in `@interface Greeter (Extra)`.
    pub name: String,
    /// The class it extends. A bind, so a category on a host class holds a host
    /// address here.
    pub class: Word,
    pub instance_methods: Vec<ObjcMethod>,
    pub class_methods: Vec<ObjcMethod>,
    pub protocols: Vec<Word>,
    pub instance_properties: Vec<ObjcProperty>,
    pub class_properties: Vec<ObjcProperty>,
}

/// A `protocol_t` as `__objc_protolist` points at it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjcProtocol {
    pub va: Word,
    pub name: String,
    /// Addresses of the protocols this one incorporates.
    pub protocols: Vec<Word>,
    pub instance_methods: Vec<ObjcMethod>,
    pub class_methods: Vec<ObjcMethod>,
    pub optional_instance_methods: Vec<ObjcMethod>,
    pub optional_class_methods: Vec<ObjcMethod>,
    pub instance_properties: Vec<ObjcProperty>,
}

/// Everything one image declares to the Objective-C runtime.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjcImage {
    pub image_info: Option<ObjcImageInfo>,
    pub classes: Vec<ObjcClass>,
    /// Classes with a `+load` method, which the runtime must realize eagerly
    /// and in link order rather than on first use.
    pub non_lazy_classes: Vec<Word>,
    pub categories: Vec<ObjcCategory>,
    /// Categories with a `+load`, same rule.
    pub non_lazy_categories: Vec<Word>,
    pub protocols: Vec<ObjcProtocol>,
    /// Addresses of the `__objc_selrefs` slots. Each holds a pointer to a
    /// selector string until the runtime uniques it and writes back a `SEL`.
    pub selector_refs: Vec<Word>,
    /// The selector word of each `__objc_msgrefs` entry (`{IMP, SEL}`), which a
    /// pre-10.9 toolchain emits for `objc_msgSend_fixup` call sites. Uniqued
    /// exactly like a `__objc_selrefs` slot.
    pub message_ref_selectors: Vec<Word>,
    /// `__objc_classrefs` slot addresses - where `[Greeter new]` reads the
    /// class from.
    pub class_refs: Vec<Word>,
    /// `__objc_superrefs` slot addresses, used by `[super ...]`.
    pub super_refs: Vec<Word>,
    /// `__objc_protorefs` slot addresses.
    pub protocol_refs: Vec<Word>,
    /// What could not be decoded, and why. Empty for every image any current
    /// toolchain produces.
    pub refusals: Vec<ObjcRefusal>,
}

impl ObjcImage {
    /// Whether the image declares anything at all to the runtime.
    pub fn is_empty(&self) -> bool {
        self.image_info.is_none()
            && self.classes.is_empty()
            && self.categories.is_empty()
            && self.protocols.is_empty()
    }

    /// Every method implementation this image defines, as guest addresses.
    ///
    /// The list an embedder needs in order to substitute each one, since an
    /// `IMP` handed to the host runtime will be called as native code.
    pub fn implementations(&self) -> impl Iterator<Item = Word> + '_ {
        let class_imps = self.classes.iter().flat_map(|c| {
            c.ro.methods
                .iter()
                .chain(c.meta.iter().flat_map(|m| m.ro.methods.iter()))
        });
        let cat_imps = self
            .categories
            .iter()
            .flat_map(|c| c.instance_methods.iter().chain(c.class_methods.iter()));
        class_imps.chain(cat_imps).map(|m| m.imp)
    }
}

/// Enumerates an image's Objective-C metadata into the report.
#[derive(Copy, Clone, Debug, Default)]
pub struct ObjcExtension;

impl<S: MachSpec> Extension<S> for ObjcExtension {
    fn name(&self) -> &'static str {
        "objc"
    }

    fn claims(&self, obj: &MachObject<'_, S>) -> bool {
        obj.sections
            .iter()
            .any(|s| s.sect_name.starts_with("__objc_"))
    }

    fn observe(
        &self,
        obj: &MachObject<'_, S>,
        mem: &dyn AddressSpace,
        slide: Word,
        report: &mut ImageReport,
    ) -> Result<()> {
        report.objc = Some(decode(obj, mem, slide)?);
        Ok(())
    }
}

/// Memory Objective-C metadata can be read from.
pub trait ObjcMemory {
    fn read(&self, addr: Word, out: &mut [u8]) -> Result<()>;
}

impl<T: AddressSpace + ?Sized> ObjcMemory for T {
    fn read(&self, addr: Word, out: &mut [u8]) -> Result<()> {
        AddressSpace::read(self, addr, out)
    }
}

/// Decode one image's Objective-C metadata, reading through `mem` post-binding.
pub fn decode<S: MachSpec>(
    obj: &MachObject<'_, S>,
    mem: &dyn AddressSpace,
    slide: Word,
) -> Result<ObjcImage> {
    let cx = Cx {
        obj: Img::new(obj),
        mem,
        slide,
        refusals: Vec::new(),
    };
    cx.run()
}

/// Decode the class whose `objc_class` is at `va`, wherever it and what it
/// points at lie: a class a runtime built, as Swift builds a generic class's
/// metadata, rather than one an image declares. `None` if `va` is null or its
/// `bits` lead nowhere.
pub fn decode_class<M: ObjcMemory + ?Sized>(mem: &M, va: Word) -> Result<Option<ObjcClass>> {
    let mut cx = Cx {
        obj: Img::anywhere(),
        mem,
        slide: 0,
        refusals: Vec::new(),
    };
    cx.class(va)
}

/// This image's sections, by name and extent.
///
/// Copied out rather than borrowed so `Cx` does not carry the object's lifetime
/// into every helper - the decode is a few hundred reads against memory, and a
/// section table is a few dozen entries.
struct Img {
    /// `(name, unslid addr, size)`, zero-sized sections dropped. `None` for
    /// metadata that is no image's, where any address is its own.
    sections: Option<Vec<(String, Word, Word)>>,
}

impl Img {
    fn new<S: MachSpec>(obj: &MachObject<'_, S>) -> Self {
        let sections = obj
            .sections
            .iter()
            .filter(|s| s.size > 0)
            .map(|s| (String::from(s.sect_name), s.addr, s.size))
            .collect();
        Img {
            sections: Some(sections),
        }
    }

    fn anywhere() -> Self {
        Img { sections: None }
    }

    /// Whether an *unslid* address lies in one of this image's sections.
    fn contains(&self, unslid: Word) -> bool {
        self.sections.as_ref().is_none_or(|sections| {
            sections
                .iter()
                .any(|&(_, addr, size)| unslid >= addr && unslid < addr.wrapping_add(size))
        })
    }
}

struct Cx<'m, M: ObjcMemory + ?Sized = dyn AddressSpace> {
    obj: Img,
    mem: &'m M,
    slide: Word,
    refusals: Vec<ObjcRefusal>,
}

impl<M: ObjcMemory + ?Sized> Cx<'_, M> {
    fn run(mut self) -> Result<ObjcImage> {
        // Ordering matters only for `image_info`, which decides how wide a
        // `category_t` is.
        let image_info = self.image_info()?;
        let has_cat_props = image_info.is_some_and(|i| i.has_category_class_properties());

        let mut classes = Vec::new();
        for va in self.pointer_section(sect::CLASSLIST)? {
            if let Some(c) = self.class(va)? {
                classes.push(c);
            }
        }
        let mut categories = Vec::new();
        for va in self.pointer_section(sect::CATLIST)? {
            if let Some(c) = self.category(va, has_cat_props)? {
                categories.push(c);
            }
        }
        let mut protocols = Vec::new();
        for va in self.pointer_section(sect::PROTOLIST)? {
            if let Some(p) = self.protocol(va)? {
                protocols.push(p);
            }
        }

        Ok(ObjcImage {
            image_info,
            classes,
            non_lazy_classes: self.pointer_section(sect::NLCLSLIST)?,
            categories,
            non_lazy_categories: self.pointer_section(sect::NLCATLIST)?,
            protocols,
            selector_refs: self.slot_addresses(sect::SELREFS),
            message_ref_selectors: self.message_ref_selectors(),
            class_refs: self.slot_addresses(sect::CLASSREFS),
            super_refs: self.slot_addresses(sect::SUPERREFS),
            protocol_refs: self.slot_addresses(sect::PROTOREFS),
            refusals: self.refusals,
        })
    }

    fn u32_at(&self, va: Word) -> Result<u32> {
        let mut b = [0u8; 4];
        self.mem.read(va, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    fn word_at(&self, va: Word) -> Result<Word> {
        let mut b = [0u8; 8];
        self.mem.read(va, &mut b)?;
        Ok(Word::from_le_bytes(b))
    }

    /// Read a NUL-terminated string. Byte at a time, because a name sits at the
    /// end of `__objc_methname` as often as not and a block read would run past
    /// the section for the last one.
    fn cstr_at(&self, va: Word) -> Result<String> {
        let mut out = Vec::new();
        for i in 0..MAX_NAME {
            let mut b = [0u8; 1];
            if self.mem.read(va.wrapping_add(i as u64), &mut b).is_err() {
                break;
            }
            if b[0] == 0 {
                break;
            }
            out.push(b[0]);
        }
        String::from_utf8(out).map_err(|_| Error::Custom("objc: name is not UTF-8"))
    }

    /// A pointer read from `at`, chased only if it lands in this image.
    ///
    /// `None` covers both "null" and "not ours", which are the two cases a
    /// caller treats identically: there is nothing here to decode. A non-null
    /// foreign pointer is recorded, because that is an edge someone will want
    /// to see.
    fn local(&mut self, at: Word, value: Word) -> Option<Word> {
        if value == 0 {
            return None;
        }
        let unslid = value.wrapping_sub(self.slide);
        if self.obj.contains(unslid) {
            Some(value)
        } else {
            self.refusals.push(ObjcRefusal::ForeignPointer {
                from: at,
                to: value,
            });
            None
        }
    }

    /// Follow the pointer stored at `at`, if it points into this image.
    fn follow(&mut self, at: Word) -> Result<Option<Word>> {
        let v = self.word_at(at)?;
        Ok(self.local(at, v))
    }

    /// A string pointed at by the pointer stored at `at`; empty if absent.
    fn follow_str(&mut self, at: Word) -> Result<String> {
        match self.follow(at)? {
            Some(p) => self.cstr_at(p),
            None => Ok(String::new()),
        }
    }

    /// The slid addresses of a pointer section's slots.
    fn slot_addresses(&self, name: &str) -> Vec<Word> {
        let mut out = Vec::new();
        for (base, count) in self.sections(name) {
            for i in 0..count {
                out.push(base.wrapping_add(i as u64 * 8));
            }
        }
        out
    }

    /// The `SEL` half of every 16-byte `message_ref_t`.
    fn message_ref_selectors(&self) -> Vec<Word> {
        let mut out = Vec::new();
        for (base, words) in self.sections(sect::MSGREFS) {
            for i in 0..words / 2 {
                out.push(base.wrapping_add(i as u64 * 16 + 8));
            }
        }
        out
    }

    /// `(slid base, entry count)` for every section with this name.
    ///
    /// A list, not an `Option`: nothing forbids an image carrying two sections
    /// of one name in different segments, and the one that would be silently
    /// dropped is exactly the one an old toolchain put in `__DATA`.
    fn sections(&self, name: &str) -> Vec<(Word, usize)> {
        self.obj
            .sections
            .iter()
            .flatten()
            .filter(|(n, _, _)| n == name)
            .map(|&(_, addr, size)| (addr.wrapping_add(self.slide), (size / 8) as usize))
            .collect()
    }

    /// The pointers a section of pointers holds.
    fn pointer_section(&mut self, name: &str) -> Result<Vec<Word>> {
        let mut out = Vec::new();
        for at in self.slot_addresses(name) {
            let v = self.word_at(at)?;
            if v != 0 {
                out.push(v);
            }
        }
        Ok(out)
    }

    fn image_info(&mut self) -> Result<Option<ObjcImageInfo>> {
        let Some(&(base, _)) = self.sections(sect::IMAGE_INFO).first() else {
            return Ok(None);
        };
        Ok(Some(ObjcImageInfo {
            version: self.u32_at(base)?,
            flags: self.u32_at(base.wrapping_add(4))?,
        }))
    }

    /// Decode a `{u32 entsize, u32 count}` list header, returning the first
    /// entry's address and the count.
    fn list_header(&mut self, at: Word, expected: u32) -> Result<Option<(Word, usize)>> {
        let entsize_and_flags = self.u32_at(at)?;
        if entsize_and_flags & METHOD_LIST_IS_SMALL != 0 {
            self.refusals.push(ObjcRefusal::SmallMethodList { at });
            return Ok(None);
        }
        let entsize = entsize_and_flags & ENTSIZE_MASK;
        if entsize != expected {
            self.refusals.push(ObjcRefusal::UnexpectedSize {
                at,
                declared: entsize,
                expected,
            });
            return Ok(None);
        }
        let count = self.u32_at(at.wrapping_add(4))? as usize;
        if count > MAX_ENTRIES {
            return Err(Error::Custom("objc: list count out of range"));
        }
        Ok(Some((at.wrapping_add(8), count)))
    }

    /// A method list at the pointer stored in `slot`, in either encoding.
    fn methods(&mut self, slot: Word) -> Result<Vec<ObjcMethod>> {
        let Some(at) = self.follow(slot)? else {
            return Ok(Vec::new());
        };
        // Which encoding is stated by the list itself, so it is asked before the
        // entry size is checked against anything: a small list's entries are
        // twelve bytes and a pointer list's are twenty-four, and comparing
        // against the wrong one refuses a list that is perfectly well formed.
        if self.u32_at(at)? & METHOD_LIST_IS_SMALL != 0 {
            return self.small_methods(at);
        }
        let Some((first, count)) = self.list_header(at, METHOD_SIZE as u32)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(count.min(64));
        for i in 0..count {
            let e = first.wrapping_add(i as u64 * METHOD_SIZE);
            out.push(ObjcMethod {
                name: self.follow_str(e)?,
                types: self.follow_str(e.wrapping_add(8))?,
                imp: self.word_at(e.wrapping_add(16))?,
            });
        }
        Ok(out)
    }

    fn small_methods(&mut self, at: Word) -> Result<Vec<ObjcMethod>> {
        let entsize = self.u32_at(at)? & ENTSIZE_MASK;
        if entsize as u64 != SMALL_METHOD_SIZE {
            self.refusals.push(ObjcRefusal::UnexpectedSize {
                at,
                declared: entsize,
                expected: SMALL_METHOD_SIZE as u32,
            });
            return Ok(Vec::new());
        }
        let count = self.u32_at(at.wrapping_add(4))? as usize;
        if count > MAX_ENTRIES {
            return Err(Error::Custom("objc: list count out of range"));
        }
        let first = at.wrapping_add(8);
        let mut out = Vec::with_capacity(count.min(64));
        for i in 0..count {
            let e = first.wrapping_add(i as u64 * SMALL_METHOD_SIZE);
            // Each target is checked against this image the way a pointer-based
            // list's is. An offset is a weaker claim than a pointer - any
            // int32 addresses something - so the check is worth more here,
            // not less.
            let sel_slot = self.relative(e)?;
            let types = self.relative(e.wrapping_add(4))?;
            let imp = self.relative(e.wrapping_add(8))?;
            out.push(ObjcMethod {
                // The slot holds the name; `follow_str` is exactly the "read a
                // pointer and chase it inside this image" step that a
                // pointer-based list's `name` field takes.
                name: match self.local(e, sel_slot) {
                    Some(slot) => self.follow_str(slot)?,
                    None => String::new(),
                },
                types: match self.local(e.wrapping_add(4), types) {
                    Some(p) => self.cstr_at(p)?,
                    None => String::new(),
                },
                imp: self.local(e.wrapping_add(8), imp).unwrap_or(0),
            });
        }
        Ok(out)
    }

    /// The address a self-relative `int32` at `at` names.
    ///
    /// Signed, and relative to `at` itself. Wrapping, because the arithmetic is
    /// on virtual addresses and a backwards offset is ordinary - a method's
    /// `types` string is in `__TEXT` and the list is in `__DATA`.
    fn relative(&mut self, at: Word) -> Result<Word> {
        let delta = self.u32_at(at)? as i32;
        Ok(at.wrapping_add(delta as i64 as Word))
    }

    fn ivars(&mut self, slot: Word) -> Result<Vec<ObjcIvar>> {
        let Some(at) = self.follow(slot)? else {
            return Ok(Vec::new());
        };
        let Some((first, count)) = self.list_header(at, IVAR_SIZE as u32)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(count.min(64));
        for i in 0..count {
            let e = first.wrapping_add(i as u64 * IVAR_SIZE);
            let offset_va = self.follow(e)?.unwrap_or(0);
            let offset = if offset_va == 0 {
                0
            } else {
                self.u32_at(offset_va)?
            };
            out.push(ObjcIvar {
                offset_va,
                offset,
                name: self.follow_str(e.wrapping_add(8))?,
                types: self.follow_str(e.wrapping_add(16))?,
                alignment: self.u32_at(e.wrapping_add(24))?,
                size: self.u32_at(e.wrapping_add(28))?,
            });
        }
        Ok(out)
    }

    fn properties(&mut self, slot: Word) -> Result<Vec<ObjcProperty>> {
        let Some(at) = self.follow(slot)? else {
            return Ok(Vec::new());
        };
        let Some((first, count)) = self.list_header(at, PROPERTY_SIZE as u32)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(count.min(64));
        for i in 0..count {
            let e = first.wrapping_add(i as u64 * PROPERTY_SIZE);
            out.push(ObjcProperty {
                name: self.follow_str(e)?,
                attributes: self.follow_str(e.wrapping_add(8))?,
            });
        }
        Ok(out)
    }

    /// A protocol list at the pointer stored in `slot`.
    ///
    /// The odd one out: a pointer-sized count, and entries that are also
    /// null-terminated. The count is authoritative; the terminator is not read.
    fn protocol_list(&mut self, slot: Word) -> Result<Vec<Word>> {
        let Some(at) = self.follow(slot)? else {
            return Ok(Vec::new());
        };
        let count = self.word_at(at)?;
        if count > MAX_ENTRIES as u64 {
            return Err(Error::Custom("objc: protocol count out of range"));
        }
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let p = self.word_at(at.wrapping_add(8 + i * 8))?;
            if p != 0 {
                out.push(p);
            }
        }
        Ok(out)
    }

    fn class_ro(&mut self, at: Word) -> Result<ClassRo> {
        let flags = self.u32_at(at)?;
        Ok(ClassRo {
            va: at,
            flags,
            instance_start: self.u32_at(at.wrapping_add(4))?,
            instance_size: self.u32_at(at.wrapping_add(8))?,
            // +12 reserved, +16 ivarLayout
            name: self.follow_str(at.wrapping_add(24))?,
            methods: self.methods(at.wrapping_add(32))?,
            protocols: self.protocol_list(at.wrapping_add(40))?,
            ivars: self.ivars(at.wrapping_add(48))?,
            // +56 weakIvarLayout
            properties: self.properties(at.wrapping_add(64))?,
            swift_initializer: match flags & RO_HAS_SWIFT_INITIALIZER {
                0 => 0,
                _ => self.word_at(at.wrapping_add(CLASS_RO_SIZE))?,
            },
        })
    }

    /// The `class_ro_t` a class's `bits` word points at.
    ///
    /// Before realization `bits` is the pointer, with the runtime's three
    /// fast flags in the low bits. After realization it would be a `class_rw_t`
    /// instead - which is why this is only ever run against an image the host
    /// runtime has not been told about.
    fn class_ro_of(&mut self, class_va: Word) -> Result<Option<ClassRo>> {
        let bits = self.word_at(class_va.wrapping_add(CLASS_BITS_OFF))?;
        let ro = bits & FAST_DATA_MASK;
        match self.local(class_va.wrapping_add(CLASS_BITS_OFF), ro) {
            Some(p) => Ok(Some(self.class_ro(p)?)),
            None => Ok(None),
        }
    }

    fn class(&mut self, va: Word) -> Result<Option<ObjcClass>> {
        if self.local(va, va).is_none() {
            return Ok(None);
        }
        let Some(ro) = self.class_ro_of(va)? else {
            return Ok(None);
        };
        let bits = self.word_at(va.wrapping_add(CLASS_BITS_OFF))?;
        let isa = self.word_at(va)?;
        // A metaclass's `isa` is the root metaclass, not a metaclass of its own.
        let meta = match self.local(va, isa).filter(|_| !ro.is_meta()) {
            Some(m) => self.class_ro_of(m)?.map(|ro| ObjcMetaclass {
                va: m,
                isa: self.word_at(m).unwrap_or(0),
                superclass: self.word_at(m.wrapping_add(8)).unwrap_or(0),
                ro,
            }),
            None => None,
        };
        Ok(Some(ObjcClass {
            va,
            isa,
            superclass: self.word_at(va.wrapping_add(8))?,
            bits,
            ro,
            meta,
        }))
    }

    fn category(&mut self, va: Word, has_class_props: bool) -> Result<Option<ObjcCategory>> {
        if self.local(va, va).is_none() {
            return Ok(None);
        }
        Ok(Some(ObjcCategory {
            va,
            name: self.follow_str(va)?,
            class: self.word_at(va.wrapping_add(8))?,
            instance_methods: self.methods(va.wrapping_add(16))?,
            class_methods: self.methods(va.wrapping_add(24))?,
            protocols: self.protocol_list(va.wrapping_add(32))?,
            instance_properties: self.properties(va.wrapping_add(40))?,
            class_properties: if has_class_props {
                self.properties(va.wrapping_add(48))?
            } else {
                Vec::new()
            },
        }))
    }

    fn protocol(&mut self, va: Word) -> Result<Option<ObjcProtocol>> {
        if self.local(va, va).is_none() {
            return Ok(None);
        }
        Ok(Some(ObjcProtocol {
            va,
            // +0 isa, always null in an unregistered protocol.
            name: self.follow_str(va.wrapping_add(8))?,
            protocols: self.protocol_list(va.wrapping_add(16))?,
            instance_methods: self.methods(va.wrapping_add(24))?,
            class_methods: self.methods(va.wrapping_add(32))?,
            optional_instance_methods: self.methods(va.wrapping_add(40))?,
            optional_class_methods: self.methods(va.wrapping_add(48))?,
            instance_properties: self.properties(va.wrapping_add(56))?,
        }))
    }
}

const _: () = {
    assert!(CLASS_BITS_OFF + 8 == CLASS_SIZE);
    assert!(CLASS_RO_SIZE == 16 + 7 * 8);
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the flat memory below starts, an address no image would claim.
    const BASE: Word = 0x7000_0000;
    const CLASS: Word = BASE;
    const META: Word = BASE + 0x40;
    const RO: Word = BASE + 0x80;
    const META_RO: Word = BASE + 0x100;
    const NAME: Word = BASE + 0x180;
    const INITIALIZER: Word = 0x1234_5678;

    /// A flat byte array at [`BASE`].
    struct Flat(Vec<u8>);

    impl ObjcMemory for Flat {
        fn read(&self, addr: Word, out: &mut [u8]) -> Result<()> {
            let start = addr.wrapping_sub(BASE) as usize;
            let src = self.0.get(start..start.wrapping_add(out.len())).ok_or(Error::OutOfBounds {
                addr,
                len: out.len(),
            })?;
            out.copy_from_slice(src);
            Ok(())
        }
    }

    impl Flat {
        fn word(&mut self, at: Word, value: Word) {
            let at = (at - BASE) as usize;
            self.0[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        fn u32(&mut self, at: Word, value: u32) {
            let at = (at - BASE) as usize;
            self.0[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    /// A class pair as Swift builds one at run time: on the heap, Swift's
    /// marker in `bits`, and its initializer after the `class_ro_t`.
    fn run_time_class() -> Flat {
        let mut m = Flat(alloc::vec![0; 0x200]);
        m.word(CLASS, META);
        m.word(CLASS + CLASS_BITS_OFF, RO | FAST_IS_SWIFT_STABLE);
        m.word(META + CLASS_BITS_OFF, META_RO);
        m.u32(RO, RO_HAS_SWIFT_INITIALIZER);
        m.word(RO + 24, NAME);
        m.word(RO + CLASS_RO_SIZE, INITIALIZER);
        m.u32(META_RO, RO_META);
        m.0[(NAME - BASE) as usize..][..6].copy_from_slice(b"Built\0");
        m
    }

    #[test]
    fn decodes_a_class_no_image_declares() {
        let class = decode_class(&run_time_class(), CLASS).unwrap().unwrap();
        assert_eq!(class.ro.name, "Built");
        assert!(class.is_swift());
        assert_eq!(class.ro.swift_initializer, INITIALIZER);
        assert_eq!(class.meta.map(|m| m.va), Some(META));
    }

    #[test]
    fn a_metaclass_decodes_without_a_metaclass_of_its_own() {
        let meta = decode_class(&run_time_class(), META).unwrap().unwrap();
        assert!(meta.ro.is_meta());
        assert!(meta.meta.is_none());
    }
}
