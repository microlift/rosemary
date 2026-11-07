use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock, RwLock};

use crate::sel::Sel;

/// A method implementation.
pub type Imp = *const c_void;

/// A method: a selector, a type encoding, and an `IMP` that can be replaced.
///
/// Leaked and never freed, so the handle a caller holds stays valid across every
/// later change to the class.
pub struct Method {
    pub sel: Sel,
    types: CString,
    imp: AtomicPtr<c_void>,
}

impl Method {
    /// The `IMP`, exactly as it was installed.
    pub fn implementation(&self) -> Imp {
        self.imp.load(Ordering::Acquire) as Imp
    }

    /// Replace the `IMP`, returning the one that was there.
    pub fn set_implementation(&self, imp: Imp) -> Imp {
        let previous = self.imp.swap(imp as *mut c_void, Ordering::AcqRel);
        bump_generation();
        previous as Imp
    }

    pub fn types(&self) -> *const c_char {
        self.types.as_ptr()
    }
}

/// An instance variable: where it sits in an instance, and how big it is.
pub struct Ivar {
    name: CString,
    /// Bytes from the start of the object. Written once, before registration.
    offset: usize,
    size: usize,
    types: CString,
}

impl Ivar {
    pub fn name(&self) -> *const c_char {
        self.name.as_ptr()
    }

    pub fn offset(&self) -> isize {
        self.offset as isize
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// The ivar's type encoding, which is what a key-value coder reads to know
    /// how wide the slot is and how to box what it finds there.
    pub fn types(&self) -> &CStr {
        &self.types
    }
}

/// The mutable half of a class.
#[derive(Default)]
struct Contents {
    methods: crate::address::AddressMap<&'static Method>,
    ivars: Vec<&'static Ivar>,
    protocols: Vec<&'static crate::protocol::Protocol>,
    properties: Vec<&'static crate::property::Property>,
    /// The ARC layouts `class_setIvarLayout` and `class_setWeakIvarLayout` set.
    ivar_layout: Option<&'static CStr>,
    weak_ivar_layout: Option<&'static CStr>,
}

/// `class_data_bits_t` flags Apple keeps in the low bits of [`Class::bits`].
///
/// Swift reads the two Swift markers off every class it meets, ours included,
/// to tell its own metadata from a plain Objective-C class.
pub const FAST_IS_SWIFT_LEGACY: usize = 1 << 0;
pub const FAST_IS_SWIFT_STABLE: usize = 1 << 1;
pub const FAST_HAS_DEFAULT_RR: usize = 1 << 2;

/// The pointer half of [`Class::bits`].
///
/// Not Apple's `FAST_DATA_MASK`, which also clears the high bits Apple's
/// address space never uses: a Linux host's user addresses can reach bit 47.
const FAST_DATA_MASK: usize = !(FAST_IS_SWIFT_LEGACY | FAST_IS_SWIFT_STABLE | FAST_HAS_DEFAULT_RR);

/// A class: Apple's `objc_class` header, whose `bits` lead to the rest.
///
/// Apple's layout because Swift class metadata begins with this header and
/// carries Swift's own fields right after it, and Swift reads the header of
/// any class it is handed. Nothing may be added past `bits`.
#[repr(C)]
pub struct Class {
    /// The metaclass. A class is an object, and this is its `isa`.
    isa: AtomicPtr<Class>,
    superclass: AtomicPtr<Class>,
    /// Apple's method cache. Never used here; it holds `bits` at Apple's offset.
    cache: [AtomicUsize; 2],
    /// The class's [`ClassData`], with the `FAST_*` flags in the low bits.
    bits: AtomicUsize,
}

/// Everything about a class past its header: Apple's `class_rw_t`.
struct ClassData {
    name: CString,
    /// Bytes an instance occupies, `isa` included. Grows as ivars are added, and
    /// is fixed at registration.
    instance_size: AtomicUsize,
    is_meta: bool,
    /// Whether `objc_registerClassPair` has run. An unregistered class is not
    /// findable by name and must not be instantiated.
    registered: AtomicUsize,
    /// Whether `+initialize` has been sent; see [`ensure_initialized`].
    initialized: AtomicBool,
    /// For a metaclass, the class it is the metaclass of.
    instances: AtomicPtr<Class>,
    /// `class_getVersion`'s answer.
    version: AtomicI32,
    contents: RwLock<Contents>,
    /// For an adopted class, the header the compiler emitted.
    compiled: Option<Compiled>,
    /// For a class whose `class_ro_t` names none, what the lazy class namer
    /// answered when first asked.
    lazy_name: OnceLock<CString>,
}

/// The header words of a compiled `objc_class` before [`adopt_pair`] took
/// it, so a class that is then refused can be handed back as it was.
#[derive(Clone, Copy)]
struct Compiled {
    isa: *mut Class,
    superclass: *mut Class,
    bits: usize,
}

// SAFETY: every field is either immutable after construction or behind a lock or
// an atomic. The raw pointers are to leaked `Class`es and `ClassData`s, or to
// compiled classes in images that are never unmapped.
unsafe impl Sync for Class {}
unsafe impl Send for Class {}
unsafe impl Sync for ClassData {}
unsafe impl Send for ClassData {}

/// A class this runtime built: the header with its data beside it, so one
/// allocation holds both.
#[repr(C)]
struct Built {
    class: Class,
    data: ClassData,
}

/// Bumped whenever a method resolution anywhere could have changed.
///
/// A caller that caches a lookup compares this; see [`generation`].
static GENERATION: AtomicUsize = AtomicUsize::new(1);

fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    // After the bump: a lookup which raced the change sees a new generation
    // and does not cache what it found.
    crate::msg::forget_cached_sends();
}

/// A counter that changes whenever any method table does.
pub fn generation() -> usize {
    GENERATION.load(Ordering::Acquire)
}

#[derive(Clone, Copy)]
struct CachedMethod {
    class: usize,
    sel: usize,
    generation: usize,
    method: Option<&'static Method>,
}

const METHOD_SLOTS: usize = 512;

thread_local! {
    static METHOD_CACHE: [std::cell::Cell<CachedMethod>; METHOD_SLOTS] = const {
        [const {
            std::cell::Cell::new(CachedMethod {
                class: 0,
                sel: 0,
                generation: 0,
                method: None,
            })
        }; METHOD_SLOTS]
    };
}

fn method_slot(class: usize, sel: usize) -> usize {
    let key = (class as u64) ^ (sel as u64).rotate_left(32);
    (crate::address::mix(key) >> 55) as usize % METHOD_SLOTS
}

/// Every class this runtime has built, so a pointer can be told from a foreign
/// one.
///
/// The isa of an object from Apple's runtime is a class this crate never made,
/// and reading it as one walks a structure that is not there. Off the send path:
/// a send already knows its receiver is the guest's, and [`is_known`] is for the
/// entry points a *host* object can reach - `CFRelease` on a `CGEventRef`, say.
const CLASS_CAPACITY: usize = 1 << 16;
const CLASS_STRIDE: usize = size_of::<Built>();
const NO_ARENA: usize = usize::MAX;

static ARENA: AtomicUsize = AtomicUsize::new(0);
static BUILT: AtomicUsize = AtomicUsize::new(0);
static BUILDING: Mutex<()> = Mutex::new(());
/// Classes outside the arena: those built past it, and compiled classes
/// adopted where an image put them. Sharded like the refcounts, because retain
/// and release ask [`is_known`] of every object.
static OUTSIDE: [RwLock<crate::address::AddressMap<()>>; OUTSIDE_SHARDS] =
    [const { RwLock::new(HashMap::with_hasher(std::hash::BuildHasherDefault::new())) };
        OUTSIDE_SHARDS];
const OUTSIDE_SHARDS: usize = 64;
/// The half-open bounds of `OUTSIDE`'s keys, so an ordinary object - almost
/// every pointer asked about - is answered without hashing.
static OUTSIDE_LO: AtomicUsize = AtomicUsize::new(usize::MAX);
static OUTSIDE_HI: AtomicUsize = AtomicUsize::new(0);

fn outside(p: usize) -> &'static RwLock<crate::address::AddressMap<()>> {
    &OUTSIDE[(crate::address::mix(p as u64) >> 58) as usize % OUTSIDE_SHARDS]
}

fn note_outside(class: &'static Class) {
    let p = std::ptr::from_ref(class) as usize;
    outside(p)
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(p, ());
    OUTSIDE_LO.fetch_min(p, Ordering::AcqRel);
    OUTSIDE_HI.fetch_max(p + 1, Ordering::AcqRel);
}

fn forget_outside(class: &'static Class) {
    let p = std::ptr::from_ref(class) as usize;
    outside(p)
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&p);
}

/// Whether `p` is a class this runtime built.
pub fn is_known(p: *const Class) -> bool {
    if p.is_null() {
        return false;
    }
    let built = BUILT.load(Ordering::Acquire);
    let offset = (p as usize).wrapping_sub(ARENA.load(Ordering::Relaxed));
    if offset < built * CLASS_STRIDE && offset.is_multiple_of(CLASS_STRIDE) {
        return true;
    }
    let p = p as usize;
    (OUTSIDE_LO.load(Ordering::Acquire)..OUTSIDE_HI.load(Ordering::Acquire)).contains(&p)
        && outside(p)
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&p)
}

fn arena_slot() -> Option<*mut Built> {
    if ARENA.load(Ordering::Relaxed) == 0 {
        let mapped = starnix::memory::anonymous(
            CLASS_CAPACITY * CLASS_STRIDE,
            libc::PROT_READ | libc::PROT_WRITE,
        );
        let base = match mapped {
            Ok(mapped) => mapped.as_ptr() as usize,
            Err(_) => NO_ARENA,
        };
        ARENA.store(base, Ordering::Relaxed);
    }
    let base = ARENA.load(Ordering::Relaxed);
    let index = BUILT.load(Ordering::Relaxed);
    (base != NO_ARENA && index < CLASS_CAPACITY)
        .then(|| (base + index * CLASS_STRIDE) as *mut Built)
}

impl Built {
    /// Point the header's `bits` at the data beside it. No flags: a class this
    /// runtime built is never Swift's.
    fn link_data(&'static self) {
        let data = std::ptr::from_ref(&self.data) as usize;
        debug_assert_eq!(data & !FAST_DATA_MASK, 0, "ClassData is word-aligned");
        self.class.bits.store(data, Ordering::Release);
    }
}

impl Class {
    /// A class, in the arena unless `at` names a compiled header to adopt.
    ///
    /// # Safety
    ///
    /// As [`adopt_pair`], for `at`.
    unsafe fn new(
        name: CString,
        superclass: *mut Class,
        is_meta: bool,
        instance_size: usize,
        at: Option<*mut Class>,
    ) -> &'static Class {
        let data = ClassData {
            name,
            instance_size: AtomicUsize::new(instance_size),
            is_meta,
            registered: AtomicUsize::new(0),
            initialized: AtomicBool::new(false),
            instances: AtomicPtr::new(std::ptr::null_mut()),
            // What Apple's runtime starts a class at when it realizes one.
            version: AtomicI32::new(if is_meta { 7 } else { 0 }),
            contents: RwLock::new(Contents::default()),
            compiled: None,
            lazy_name: OnceLock::new(),
        };
        match at {
            // SAFETY: the caller's obligation.
            Some(header) => unsafe { Self::adopt(header, superclass, data) },
            None => Self::build(superclass, data),
        }
    }

    /// Take over a compiled header, as libobjc realizes an image's class where
    /// the image put it. The cache words are left as compiled; the Swift
    /// markers in `bits` are kept, as Swift reads them back.
    ///
    /// # Safety
    ///
    /// As [`adopt_pair`].
    unsafe fn adopt(
        header: *mut Class,
        superclass: *mut Class,
        mut data: ClassData,
    ) -> &'static Class {
        // SAFETY: the caller's obligation.
        let class: &'static Class = unsafe { &*header };
        let bits = class.bits.load(Ordering::Acquire);
        data.compiled = Some(Compiled {
            isa: class.isa.load(Ordering::Acquire),
            superclass: class.superclass.load(Ordering::Acquire),
            bits,
        });
        let data = std::ptr::from_ref(Box::leak(Box::new(data))) as usize;
        let swift = bits & (FAST_IS_SWIFT_LEGACY | FAST_IS_SWIFT_STABLE);
        class.superclass.store(superclass, Ordering::Release);
        class.bits.store(data | swift, Ordering::Release);
        note_outside(class);
        class
    }

    /// Hand a compiled header back as [`Class::adopt`] found it.
    fn restore(&'static self, compiled: Compiled) {
        forget_outside(self);
        self.isa.store(compiled.isa, Ordering::Release);
        self.superclass
            .store(compiled.superclass, Ordering::Release);
        self.bits.store(compiled.bits, Ordering::Release);
    }

    fn build(superclass: *mut Class, data: ClassData) -> &'static Class {
        let built = Built {
            class: Class {
                isa: AtomicPtr::new(std::ptr::null_mut()),
                superclass: AtomicPtr::new(superclass),
                cache: [AtomicUsize::new(0), AtomicUsize::new(0)],
                // Pointed at `data` once `built` has its final address.
                bits: AtomicUsize::new(0),
            },
            data,
        };
        let _building = BUILDING.lock().unwrap_or_else(|e| e.into_inner());
        match arena_slot() {
            Some(slot) => {
                // SAFETY: an unused, aligned slot of a reservation that is never
                // unmapped, published only once written.
                unsafe { slot.write(built) };
                // SAFETY: as above.
                let built: &'static Built = unsafe { &*slot };
                built.link_data();
                BUILT.fetch_add(1, Ordering::Release);
                &built.class
            }
            None => {
                let built: &'static Built = Box::leak(Box::new(built));
                built.link_data();
                note_outside(&built.class);
                &built.class
            }
        }
    }

    /// The [`ClassData`] `bits` points at.
    #[inline]
    fn data(&self) -> &ClassData {
        let bits = self.bits.load(Ordering::Acquire);
        // SAFETY: every `Class` this runtime hands out has had `bits` pointed
        // at a leaked `ClassData` before it was published, and never cleared.
        unsafe { &*((bits & FAST_DATA_MASK) as *const ClassData) }
    }

    /// The raw `class_data_bits_t`, flags included.
    pub fn bits(&self) -> usize {
        self.bits.load(Ordering::Acquire)
    }

    pub fn name(&self) -> *const c_char {
        self.name_c().as_ptr()
    }

    pub fn name_str(&self) -> &str {
        self.name_c().to_str().unwrap_or("<non-utf8 class name>")
    }

    /// The name, which a class Swift built at run time - a generic class -
    /// gets from the lazy class namer on first ask, as Apple's `mangledName`
    /// does. A metaclass is named as its class.
    fn name_c(&self) -> &CStr {
        let data = self.data();
        if !data.name.is_empty() {
            return &data.name;
        }
        let instances = data.instances.load(Ordering::Acquire);
        if data.is_meta && !instances.is_null() {
            // SAFETY: a leaked or adopted class, set when the pair was made.
            return unsafe { &*instances }.name_c();
        }
        data.lazy_name
            .get_or_init(|| crate::api::lazy_class_name(self))
    }

    pub fn is_meta(&self) -> bool {
        self.data().is_meta
    }

    pub fn is_registered(&self) -> bool {
        self.data().registered.load(Ordering::Acquire) != 0
    }

    pub fn superclass(&self) -> Option<&'static Class> {
        let p = self.superclass.load(Ordering::Acquire);
        // SAFETY: null, or a leaked `Class` that outlives the process.
        (!p.is_null()).then(|| unsafe { &*(p as *const Class) })
    }

    /// The metaclass, which is where class methods live.
    pub fn metaclass(&self) -> &'static Class {
        let p = self.isa.load(Ordering::Acquire);
        assert!(!p.is_null(), "every class pair has a metaclass");
        // SAFETY: set at allocation to a leaked `Class`, and never cleared.
        unsafe { &*(p as *const Class) }
    }

    pub fn instance_size(&self) -> usize {
        self.data().instance_size.load(Ordering::Acquire)
    }

    /// This class and every superclass, nearest first.
    pub fn chain(&'static self) -> impl Iterator<Item = &'static Class> {
        let mut next = Some(self);
        std::iter::from_fn(move || {
            let current = next?;
            next = current.superclass();
            Some(current)
        })
    }

    /// The method this class itself declares for `sel`, ignoring superclasses.
    pub fn own_method(&self, sel: Sel) -> Option<&'static Method> {
        crate::sel::validate(sel);
        self.data()
            .contents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .methods
            .get(&(sel as usize))
            .copied()
    }

    /// Every method this class declares itself, in no particular order.
    ///
    /// For a caller checking a whole class table at once - the encoding guard in
    /// `rosemary` - rather than for dispatch, which asks by selector.
    pub fn own_methods(&'static self) -> Vec<&'static Method> {
        self.data()
            .contents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .methods
            .values()
            .copied()
            .collect()
    }

    /// The method `sel` resolves to, searching superclasses.
    pub fn method(&'static self, sel: Sel) -> Option<&'static Method> {
        let class = std::ptr::from_ref(self) as usize;
        let generation = generation();
        let slot = method_slot(class, sel as usize);
        let cached = METHOD_CACHE.with(|cache| cache[slot].get());
        if cached.class == class && cached.sel == sel as usize && cached.generation == generation {
            return cached.method;
        }
        // Misses are cached too: respondsToSelector: and forwarding probes ask
        // for methods that are not there far more often than once.
        let method = self.chain().find_map(|c| c.own_method(sel));
        METHOD_CACHE.with(|cache| {
            cache[slot].set(CachedMethod {
                class,
                sel: sel as usize,
                generation,
                method,
            })
        });
        method
    }

    /// Install `imp` for `sel`, failing if this class already declares it.
    ///
    /// `false` rather than an error, because that is what the C API returns and
    /// because a duplicate is the guest declaring the same selector twice.
    pub fn add_method(&self, sel: Sel, imp: Imp, types: &CStr) -> bool {
        crate::sel::validate(sel);
        if crate::debug::enabled() && imp.is_null() {
            crate::fail!(
                "objc debug: class_addMethod on {} was given a null IMP",
                self.name_str()
            );
        }
        let mut contents = self
            .data()
            .contents
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if contents.methods.contains_key(&(sel as usize)) {
            return false;
        }
        let method: &'static Method = Box::leak(Box::new(Method {
            sel,
            types: CString::from(types),
            imp: AtomicPtr::new(imp as *mut c_void),
        }));
        crate::debug::note_handle(method, "method");
        contents.methods.insert(sel as usize, method);
        drop(contents);
        bump_generation();
        true
    }

    /// Install `imp` for `sel` whether or not it is already declared, returning
    /// the `IMP` that was there.
    pub fn replace_method(&self, sel: Sel, imp: Imp, types: &CStr) -> Imp {
        crate::sel::validate(sel);
        if crate::debug::enabled() && imp.is_null() {
            crate::fail!(
                "objc debug: class_replaceMethod on {} was given a null IMP",
                self.name_str()
            );
        }
        if let Some(existing) = self.own_method(sel) {
            return existing.set_implementation(imp);
        }
        self.add_method(sel, imp, types);
        std::ptr::null()
    }

    /// Append an ivar. Only before registration, and never on a metaclass.
    pub fn add_ivar(&self, name: &CStr, size: usize, alignment: u8, types: &CStr) -> bool {
        // `alignment` is log2, as `class_addIvar` documents. Apple caps it too.
        let align = 1usize << alignment.min(15);
        let start = self.data().instance_size.load(Ordering::Acquire);
        self.place_ivar(name, start.next_multiple_of(align), size, types)
    }

    /// Install an ivar at `offset` - for a compiled class, where the compiler
    /// put it, moved by [`ivar_slide`]. The instance grows to hold it. Only
    /// before registration, and never on a metaclass.
    pub fn place_ivar(&self, name: &CStr, offset: usize, size: usize, types: &CStr) -> bool {
        if self.is_registered() || self.data().is_meta {
            return false;
        }
        let mut contents = self
            .data()
            .contents
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if contents.ivars.iter().any(|i| i.name.as_c_str() == name) {
            return false;
        }
        let ivar: &'static Ivar = Box::leak(Box::new(Ivar {
            name: CString::from(name),
            offset,
            size,
            types: CString::from(types),
        }));
        crate::debug::note_handle(ivar, "ivar");
        contents.ivars.push(ivar);
        self.data()
            .instance_size
            .fetch_max(offset + size, Ordering::AcqRel);
        true
    }

    /// Set the instance size a compiled class declares, moved by
    /// [`ivar_slide`]. Only before registration, and never smaller than the
    /// ivars already placed.
    pub fn set_instance_size(&self, size: usize) -> bool {
        if self.is_registered() {
            return false;
        }
        self.data().instance_size.fetch_max(size, Ordering::AcqRel);
        true
    }

    /// The ivars this class declares itself, in the order they were added.
    pub fn own_ivars(&self) -> Vec<&'static Ivar> {
        self.data()
            .contents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .ivars
            .clone()
    }

    pub fn version(&self) -> i32 {
        self.data().version.load(Ordering::Acquire)
    }

    pub fn set_version(&self, version: i32) {
        self.data().version.store(version, Ordering::Release);
    }

    /// The strong (`weak` false) or weak ivar layout, if one was set.
    pub fn ivar_layout(&self, weak: bool) -> Option<&'static CStr> {
        let contents = self
            .data()
            .contents
            .read()
            .unwrap_or_else(|e| e.into_inner());
        match weak {
            true => contents.weak_ivar_layout,
            false => contents.ivar_layout,
        }
    }

    /// Set a layout. Only before registration, as with ivars: the layout
    /// describes them.
    pub fn set_ivar_layout(&self, weak: bool, layout: Option<&CStr>) -> bool {
        if self.is_registered() {
            return false;
        }
        let layout = layout.map(|l| &*Box::leak(CString::from(l).into_boxed_c_str()));
        let mut contents = self
            .data()
            .contents
            .write()
            .unwrap_or_else(|e| e.into_inner());
        match weak {
            true => contents.weak_ivar_layout = layout,
            false => contents.ivar_layout = layout,
        }
        true
    }

    /// The ivar this class or a superclass declares under `name`.
    pub fn ivar(&'static self, name: &CStr) -> Option<&'static Ivar> {
        self.chain().find_map(|c| {
            c.data()
                .contents
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .ivars
                .iter()
                .find(|i| i.name.as_c_str() == name)
                .copied()
        })
    }

    /// Whether this class or a superclass declares this exact ivar handle.
    pub fn contains_ivar(&'static self, ivar: *const Ivar) -> bool {
        self.chain().any(|class| {
            class
                .data()
                .contents
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .ivars
                .iter()
                .any(|known| std::ptr::eq(*known, ivar))
        })
    }

    /// Add or replace a `@property`.
    ///
    /// Unlike an ivar this is allowed after registration, because a category
    /// carries properties and a category is applied to a class that is already
    /// there. Replacing rather than appending a second of the same name: two
    /// entries would make `class_copyPropertyList` report a property twice and
    /// `class_getProperty` answer with whichever came first.
    pub fn set_property(&self, name: &CStr, attributes: &CStr) {
        let mut contents = self
            .data()
            .contents
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let property = crate::property::make(name, attributes);
        match contents.properties.iter().position(|p| p.name_c() == name) {
            Some(at) => contents.properties[at] = property,
            None => contents.properties.push(property),
        }
    }

    /// The properties this class declares, not counting a superclass's - which
    /// is what `class_copyPropertyList` answers.
    pub fn properties(&self) -> Vec<&'static crate::property::Property> {
        self.data()
            .contents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .properties
            .clone()
    }

    /// The property this class or a superclass declares under `name`.
    pub fn property(&'static self, name: &CStr) -> Option<&'static crate::property::Property> {
        self.chain().find_map(|c| {
            c.data()
                .contents
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .properties
                .iter()
                .find(|p| p.name_c() == name)
                .copied()
        })
    }

    pub fn add_protocol(&self, protocol: &'static crate::protocol::Protocol) -> bool {
        let mut contents = self
            .data()
            .contents
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if contents
            .protocols
            .iter()
            .any(|p| std::ptr::eq(*p, protocol))
        {
            return false;
        }
        contents.protocols.push(protocol);
        true
    }

    /// The protocols this class adopts itself, in the order it adopted them.
    pub fn own_protocols(&self) -> Vec<&'static crate::protocol::Protocol> {
        self.data()
            .contents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .protocols
            .clone()
    }

    /// Whether this class or a superclass adopts `protocol`.
    pub fn conforms_to(&'static self, protocol: &crate::protocol::Protocol) -> bool {
        self.chain().any(|c| {
            c.data()
                .contents
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .protocols
                .iter()
                .any(|p| std::ptr::eq(*p, protocol) || p.conforms_to(protocol))
        })
    }
}

/// Classes whose `+initialize` is running, and the thread running each.
static INITIALIZING: Mutex<Vec<(usize, std::thread::ThreadId)>> = Mutex::new(Vec::new());
static INITIALIZED: std::sync::Condvar = std::sync::Condvar::new();

/// Send `+initialize` to `class` - its superclasses' first - unless it has
/// been already, as Apple's runtime does before the first message to a class
/// or to an instance of it.
///
/// Sent, so a class with no `+initialize` of its own runs its superclass's
/// with itself as the receiver, which is what a guard of
/// `if (self == [Foo class])` is written against. A message the class's own
/// `+initialize` sends to the class goes straight through; a message from
/// another thread waits until it has returned.
/// Whether `+initialize` has finished for `class`, or for the class whose
/// metaclass it is.
pub(crate) fn is_initialized(class: &'static Class) -> bool {
    match class.is_meta() {
        true => {
            let instances = class.data().instances.load(Ordering::Acquire);
            // SAFETY: a leaked class, set when the pair was allocated.
            !instances.is_null()
                && unsafe { &*(instances as *const Class) }
                    .data()
                    .initialized
                    .load(Ordering::Acquire)
        }
        false => class.data().initialized.load(Ordering::Acquire),
    }
}

#[inline]
pub fn ensure_initialized(class: &'static Class) {
    // Every message send asks, and the answer is nearly always yes.
    if !class.data().initialized.load(Ordering::Acquire) {
        initialize(class);
    }
}

#[cold]
fn initialize(class: &'static Class) {
    let class = match class.is_meta() {
        true => {
            let instances = class.data().instances.load(Ordering::Acquire);
            match instances.is_null() {
                true => return,
                // SAFETY: a leaked class, set when the pair was allocated.
                false => unsafe { &*(instances as *const Class) },
            }
        }
        false => class,
    };
    if class.data().initialized.load(Ordering::Acquire) || !class.is_registered() {
        return;
    }
    if let Some(superclass) = class.superclass() {
        ensure_initialized(superclass);
    }
    let address = ptr(class) as usize;
    let me = std::thread::current().id();
    {
        let mut running = INITIALIZING.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if class.data().initialized.load(Ordering::Acquire) {
                return;
            }
            match running.iter().find(|(c, _)| *c == address) {
                Some((_, thread)) if *thread == me => return,
                Some(_) => {
                    running = INITIALIZED.wait(running).unwrap_or_else(|e| e.into_inner());
                }
                None => {
                    running.push((address, me));
                    break;
                }
            }
        }
    }
    let sel = crate::sel::get("initialize");
    if let Some(method) = class.metaclass().method(sel) {
        // SAFETY: `+initialize` is `void (Class, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(*mut Class, Sel) =
                std::mem::transmute(method.implementation());
            f(ptr(class), sel);
        }
    }
    class.data().initialized.store(true, Ordering::Release);
    INITIALIZING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(c, _)| *c != address);
    INITIALIZED.notify_all();
}

/// A class by name, or `None` if none is registered.
///
/// Does not bootstrap: [`crate::bootstrap`] itself reaches this while installing
/// `NSObject`, and a `Once` re-entered from inside its own initializer does not
/// return.
pub fn lookup(name: &CStr) -> Option<&'static Class> {
    let address = name.as_ptr() as usize;
    let generation = NAMED.load(Ordering::Acquire);
    let slot = (crate::address::mix(address as u64) >> 56) as usize % LOOKUP_SLOTS;
    let (at, cached_generation, cached) = RECENT_LOOKUPS.with(|recent| recent[slot].get());
    if at == address
        && cached_generation == generation
        && let Some(class) = cached
        && class.data().name.as_c_str() == name
    {
        return Some(class);
    }
    let class = CLASSES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(name.to_str().ok()?)
        .copied()?;
    RECENT_LOOKUPS.with(|recent| recent[slot].set((address, generation, Some(class))));
    Some(class)
}

/// A class by name as the C API looks one up: [`lookup`], then the
/// `getClass` hook Swift installs, which finds a Swift class by the name its
/// image does not register it under.
pub fn look_up(name: &CStr) -> Option<&'static Class> {
    lookup(name).or_else(|| crate::api::class_from_hook(name))
}

static NAMED: AtomicUsize = AtomicUsize::new(1);

const LOOKUP_SLOTS: usize = 128;

thread_local! {
    static RECENT_LOOKUPS: [std::cell::Cell<(usize, usize, Option<&'static Class>)>; LOOKUP_SLOTS] =
        const { [const { std::cell::Cell::new((0, 0, None)) }; LOOKUP_SLOTS] };
}

/// Every registered class, by name.
static CLASSES: LazyLock<RwLock<HashMap<String, &'static Class>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Every registered class, in no particular order.
pub fn registered() -> Vec<&'static Class> {
    CLASSES
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .copied()
        .collect()
}

/// Guards a class pair being built, so two threads cannot allocate the same name.
static ALLOCATION: Mutex<()> = Mutex::new(());

/// Allocate an unregistered class and its metaclass.
///
/// `None` when the name is taken, which is what `objc_allocateClassPair`'s null
/// return means. `superclass` of `None` makes a root class.
pub fn allocate_pair(
    superclass: Option<&'static Class>,
    name: &CStr,
    extra: usize,
) -> Option<&'static Class> {
    // SAFETY: no compiled header is named.
    unsafe { pair(superclass, name, extra, None) }
}

/// [`allocate_pair`], made of the compiled `objc_class` at `class` and its
/// metaclass at `meta`, as libobjc realizes an image's classes in place: the
/// address the image gave the class stays the class. A `meta` of `None` gets a
/// metaclass of this runtime's own.
///
/// `None` as for [`allocate_pair`], and for a header not aligned as one.
///
/// # Safety
///
/// `class` and `meta` are writable `objc_class` headers that are never
/// unmapped, and nothing else writes them while this runtime owns them.
pub unsafe fn adopt_pair(
    class: *mut Class,
    meta: Option<*mut Class>,
    superclass: Option<&'static Class>,
    name: &CStr,
) -> Option<&'static Class> {
    let aligned = |p: *mut Class| !p.is_null() && p.is_aligned();
    if !aligned(class) || meta.is_some_and(|m| !aligned(m)) {
        return None;
    }
    // SAFETY: the caller's obligation.
    unsafe { pair(superclass, name, 0, Some((class, meta))) }
}

/// # Safety
///
/// As [`adopt_pair`], for `at`.
unsafe fn pair(
    superclass: Option<&'static Class>,
    name: &CStr,
    extra: usize,
    at: Option<(*mut Class, Option<*mut Class>)>,
) -> Option<&'static Class> {
    let _guard = ALLOCATION.lock().unwrap_or_else(|e| e.into_inner());
    // A class with no name yet takes none from another.
    if !name.is_empty() && lookup(name).is_some() {
        return None;
    }
    let name = CString::from(name);

    // An instance starts where the superclass's ended - at `isa` for a root.
    let instance_start = superclass.map_or(size_of::<usize>(), |s| s.instance_size());
    // SAFETY: the caller's obligation.
    let class = unsafe {
        Class::new(
            name.clone(),
            superclass.map_or(std::ptr::null_mut(), ptr),
            false,
            instance_start + extra,
            at.map(|(class, _)| class),
        )
    };

    // The metaclass's superclass is the superclass's metaclass, and a root
    // metaclass's superclass is the root class itself - which is what makes
    // `+alloc` findable from a metaclass.
    let meta_super = match superclass {
        Some(s) => ptr(s.metaclass()),
        None => ptr(class),
    };
    // SAFETY: as above.
    let meta = unsafe {
        Class::new(
            name,
            meta_super,
            true,
            size_of::<usize>(),
            at.and_then(|(_, meta)| meta),
        )
    };

    // Every metaclass's `isa` is the *root* metaclass, and the root metaclass's
    // is itself. That is what stops `object_getClass` recursing forever.
    let root_meta = match superclass {
        Some(s) => root_metaclass(s),
        None => meta,
    };
    meta.isa.store(ptr(root_meta), Ordering::Release);
    meta.data().instances.store(ptr(class), Ordering::Release);
    class.isa.store(ptr(meta), Ordering::Release);
    Some(class)
}

/// How far a compiled class's ivars move, as Apple's `moveIvars` does it: not
/// at all unless the superclass's instances end past `instance_start`, where
/// the compiler began this class's ivars, and then by that overlap rounded up
/// to the strictest ivar alignment so every ivar stays aligned.
pub fn ivar_slide(superclass_size: usize, instance_start: usize, max_alignment: usize) -> usize {
    match superclass_size.checked_sub(instance_start) {
        Some(overlap) if overlap > 0 => overlap.next_multiple_of(max_alignment.max(1)),
        _ => 0,
    }
}

/// Reparent `class`, and its metaclass with it, returning the old superclass.
///
/// Apple's has been deprecated since the runtime it shipped in. Swift still
/// calls it, to slot its own `NSArray`-shaped bases under Foundation's once
/// Foundation is loaded.
pub fn set_superclass(
    class: &'static Class,
    superclass: Option<&'static Class>,
) -> Option<&'static Class> {
    let _guard = ALLOCATION.lock().unwrap_or_else(|e| e.into_inner());
    let previous = class.superclass();
    // As at allocation: a root's metaclass inherits from the root itself.
    let meta_super = superclass.map_or(ptr(class), |s| ptr(s.metaclass()));
    class.superclass.store(
        superclass.map_or(std::ptr::null_mut(), ptr),
        Ordering::Release,
    );
    class
        .metaclass()
        .superclass
        .store(meta_super, Ordering::Release);
    bump_generation();
    previous
}

/// The metaclass at the top of `class`'s chain.
fn root_metaclass(class: &'static Class) -> &'static Class {
    let mut root = class;
    while let Some(next) = root.superclass() {
        root = next;
    }
    root.metaclass()
}

/// Make `class` findable by name and instantiable.
pub fn register_pair(class: &'static Class) {
    if crate::debug::enabled() && class.is_registered() {
        crate::fail!(
            "objc debug: objc_registerClassPair called twice for {}",
            class.name_str()
        );
    }
    if class.data().registered.swap(1, Ordering::AcqRel) != 0 {
        return;
    }
    class
        .metaclass()
        .data()
        .registered
        .store(1, Ordering::Release);
    // A class named only lazily is found by its address, as Apple's is.
    let name = &class.data().name;
    if !name.is_empty() {
        with_classes(|classes| {
            classes.insert(name.to_string_lossy().into_owned(), class);
        });
    }
}

/// Forget a class pair that was allocated and never registered.
pub fn dispose_pair(class: &'static Class) {
    if crate::debug::enabled() && class.is_registered() {
        crate::fail!(
            "objc debug: objc_disposeClassPair called for registered class {}",
            class.name_str()
        );
    }
    if class.is_registered() && !class.data().name.is_empty() {
        with_classes(|classes| {
            classes.remove(class.name_str());
        });
    }
    // A compiled pair goes back to being the image's own. The metaclass first:
    // the class's `isa` is how it is found.
    let meta = class.metaclass();
    for c in [meta, class] {
        if let Some(compiled) = c.data().compiled {
            c.restore(compiled);
        }
    }
    // The `ClassData` stays leaked: a pointer to it may have escaped, and a
    // dangling class pointer is far worse than a few hundred bytes.
}

fn with_classes<R>(body: impl FnOnce(&mut HashMap<String, &'static Class>) -> R) -> R {
    let mut classes = CLASSES.write().unwrap_or_else(|e| e.into_inner());
    let result = body(&mut classes);
    NAMED.fetch_add(1, Ordering::Release);
    result
}

/// A `&'static Class` as the raw pointer the C API passes around.
pub fn ptr(class: &'static Class) -> *mut Class {
    class as *const Class as *mut Class
}

/// The class a raw pointer names.
///
/// A compiled class nothing has realized yet, such as one Swift finishes on
/// first use, is realized now, as libobjc does on a class's first message;
/// anything else this runtime does not have is no class.
///
/// # Safety
///
/// `p` must be null or a class pointer.
pub unsafe fn from_ptr(p: *const Class) -> Option<&'static Class> {
    if p.is_null() {
        return None;
    }
    if is_known(p) {
        // SAFETY: a class this runtime built or adopted.
        return Some(unsafe { &*p });
    }
    match realize(p, std::ptr::null()) {
        Some(class) => Some(class),
        None if crate::debug::enabled() => crate::fail!("objc debug: invalid class handle {p:p}"),
        None => None,
    }
}

/// Realize the class at `class`, first touched as the `isa` of `object`, if
/// there is one: a compiled metaclass is realized with its class, which only
/// such an object can name. `None` for anything that is not a compiled class
/// waiting.
pub fn realize(class: *const Class, object: *const c_void) -> Option<&'static Class> {
    if class.is_null() {
        return None;
    }
    let realized = crate::api::realize_compiled_class(
        class as *mut c_void,
        crate::api::Realize::FirstTouch {
            object: object as *mut c_void,
        },
    );
    // SAFETY: a class this runtime adopted or built.
    is_known(realized as *const Class).then(|| unsafe { &*(realized as *const Class) })
}

/// The `IMP` `sel` resolves to on `class`, or null.
///
/// Returns the resolved `IMP` itself. A caller may cache it and re-probe to
/// notice a swizzle, which is exactly what `rosemary`'s dispatch plan does, and
/// only holds because nothing is wrapped on the way out.
pub fn method_implementation(class: &'static Class, sel: Sel) -> Imp {
    class
        .method(sel)
        .map_or(std::ptr::null(), |m| m.implementation())
}

/// `OS_object`: the class the runtime's own non-Objective-C objects wear.
///
/// Darwin's dispatch objects are Objective-C objects - `dispatch_queue_t` is
/// `id`-compatible, and ARC emits `objc_storeStrong`, `objc_retain` and
/// `objc_release` for one - so a guest that puts a queue in a strong variable
/// hands it to this runtime's ARC. Reaching ARC with a null `isa` is
/// indistinguishable from reaching it with zeroed garbage, which is what the
/// hardening's `isa` check is there to catch; wearing a class of this
/// runtime's own is what tells the two apart.
///
/// A root class with no methods is all that is needed. It carries no
/// behaviour: a message actually sent to one of these still fails, but by
/// selector rather than on a null `isa`.
pub fn os_object_class() -> &'static Class {
    static CLASS: OnceLock<&'static Class> = OnceLock::new();
    CLASS.get_or_init(|| {
        let class = allocate_pair(None, c"OS_object", 0)
            .unwrap_or_else(|| crate::fail!("objc: the OS_object class name is already taken"));
        register_pair(class);
        class
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apple's `objc_class`: `isa`, `superclass`, `cache` (two words), `bits`.
    const OBJC_CLASS_BITS_OFFSET: usize = 4 * size_of::<usize>();
    const OBJC_CLASS_SIZE: usize = 5 * size_of::<usize>();

    #[test]
    fn class_header_has_apples_objc_class_layout() {
        assert_eq!(std::mem::offset_of!(Class, isa), 0);
        assert_eq!(std::mem::offset_of!(Class, superclass), size_of::<usize>());
        assert_eq!(std::mem::offset_of!(Class, bits), OBJC_CLASS_BITS_OFFSET);
        assert_eq!(size_of::<Class>(), OBJC_CLASS_SIZE);
    }

    #[test]
    fn a_built_class_does_not_read_as_swift_metadata() {
        crate::bootstrap();
        let nsobject = lookup(c"NSObject").expect("NSObject");
        // An odd instance size used to land where Swift reads the flags.
        let class = allocate_pair(Some(nsobject), c"RosemaryOddSizedClass", 1).expect("unused");
        register_pair(class);
        for c in [nsobject, class, class.metaclass()] {
            let bits = c.bits();
            assert_eq!(bits & (FAST_IS_SWIFT_LEGACY | FAST_IS_SWIFT_STABLE), 0);
            assert_ne!(bits & FAST_DATA_MASK, 0);
        }
        assert_eq!(class.instance_size(), nsobject.instance_size() + 1);
    }

    /// What a compiler leaves in a class's cache words: `_objc_empty_cache`.
    const EMPTY_CACHE: usize = 0x1000;
    /// A `class_ro_t` address, as `bits` holds it before realization.
    const COMPILED_RO: usize = 0x2000;

    /// A compiled `objc_class` header, as an image holds one.
    fn compiled_header(bits: usize) -> *mut Class {
        Box::into_raw(Box::new(Class {
            isa: AtomicPtr::new(std::ptr::null_mut()),
            superclass: AtomicPtr::new(std::ptr::null_mut()),
            cache: [AtomicUsize::new(EMPTY_CACHE), AtomicUsize::new(0)],
            bits: AtomicUsize::new(bits),
        }))
    }

    fn adopt_under_nsobject(name: &CStr, bits: usize) -> (*mut Class, *mut Class, &'static Class) {
        crate::bootstrap();
        let nsobject = lookup(c"NSObject").expect("NSObject");
        let (header, meta) = (compiled_header(bits), compiled_header(COMPILED_RO));
        // SAFETY: both headers are leaked and written by nothing else.
        let class =
            unsafe { adopt_pair(header, Some(meta), Some(nsobject), name) }.expect("unused");
        (header, meta, class)
    }

    #[test]
    fn an_adopted_class_is_its_compiled_header() {
        let (header, meta, class) = adopt_under_nsobject(c"RosemaryAdoptedClass", COMPILED_RO);
        let sel = crate::sel::get("rosemaryAdoptedAnswer");
        assert!(class.add_method(sel, std::ptr::dangling(), c"Q16@0:8"));
        register_pair(class);

        assert!(std::ptr::eq(class, header));
        assert!(std::ptr::eq(class.metaclass(), meta));
        assert!(std::ptr::eq(
            lookup(c"RosemaryAdoptedClass").unwrap(),
            header
        ));
        assert!(is_known(header) && is_known(meta));
        assert!(class.method(sel).is_some());
        assert_eq!(class.cache[0].load(Ordering::Acquire), EMPTY_CACHE);
        let instance = crate::object::create_instance(class, 0);
        // SAFETY: a live instance, whose first word is its class.
        assert_eq!(unsafe { *(instance as *const *mut Class) }, header);
    }

    #[test]
    fn an_adopted_class_keeps_its_swift_marker() {
        let bits = COMPILED_RO | FAST_IS_SWIFT_STABLE;
        let (_, _, class) = adopt_under_nsobject(c"RosemaryAdoptedSwiftClass", bits);
        assert_eq!(class.bits() & FAST_IS_SWIFT_STABLE, FAST_IS_SWIFT_STABLE);
        assert_eq!(class.name_str(), "RosemaryAdoptedSwiftClass");
    }

    #[test]
    fn ivars_slide_only_past_a_superclass_that_outgrew_them() {
        const WORD: usize = size_of::<usize>();
        assert_eq!(ivar_slide(2 * WORD, 2 * WORD, WORD), 0);
        assert_eq!(ivar_slide(WORD, 2 * WORD, WORD), 0);
        assert_eq!(ivar_slide(2 * WORD + 4, 2 * WORD, WORD), WORD);
        assert_eq!(ivar_slide(2 * WORD + 4, 2 * WORD, 4), 4);
    }

    #[test]
    fn a_root_class_declaring_isa_keeps_its_compiled_layout() {
        const WORD: usize = size_of::<usize>();
        let root = allocate_pair(None, c"RosemaryCompiledRoot", 0).expect("unused");
        assert!(root.place_ivar(c"isa", 0, WORD, c"#"));
        assert!(root.place_ivar(c"refCounts", WORD, WORD, c"Q"));
        assert!(root.set_instance_size(2 * WORD));
        register_pair(root);

        assert_eq!(root.ivar(c"isa").unwrap().offset(), 0);
        assert_eq!(root.ivar(c"refCounts").unwrap().offset(), WORD as isize);
        assert_eq!(root.instance_size(), 2 * WORD);
    }

    /// A compiled class's `class_ro_t`, as far as realization reads one: its
    /// flags word.
    fn compiled_ro(flags: u32) -> usize {
        Box::into_raw(Box::new(flags)) as usize
    }

    /// Names for the classes [`adopt_compiled`] is handed, by header.
    static COMPILED_NAMES: Mutex<Vec<(usize, &'static CStr)>> = Mutex::new(Vec::new());
    /// The compiled metaclasses the tests made.
    static COMPILED_METAS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

    /// `class_ro_t::flags` bit marking a metaclass.
    const RO_META: u32 = 1 << 0;

    /// The embedder's realizer, for the tests: adopt any compiled class the
    /// tests named under `NSObject`, and realize a metaclass as the object it
    /// was met through.
    fn adopt_compiled(cls: *mut c_void, how: crate::api::Realize) -> *mut c_void {
        let header = cls as *mut Class;
        let named = |p: *mut c_void| {
            COMPILED_NAMES
                .lock()
                .unwrap()
                .iter()
                .find_map(|&(h, name)| (h == p as usize).then_some(name))
        };
        if COMPILED_METAS.lock().unwrap().contains(&(cls as usize)) {
            let crate::api::Realize::FirstTouch { object } = how else {
                return std::ptr::null_mut();
            };
            if named(object).is_none() {
                return std::ptr::null_mut();
            }
            let first_touch = crate::api::Realize::FirstTouch {
                object: std::ptr::null_mut(),
            };
            adopt_compiled(object, first_touch);
            return match is_known(header) {
                true => cls,
                false => std::ptr::null_mut(),
            };
        }
        let Some(name) = named(cls) else {
            return std::ptr::null_mut();
        };
        let nsobject = lookup(c"NSObject");
        // SAFETY: as above; nothing else writes the header.
        let meta = unsafe { (*header).isa.load(Ordering::Acquire) };
        let Some(class) = (unsafe { adopt_pair(header, Some(meta), nsobject, name) }) else {
            return std::ptr::null_mut();
        };
        register_pair(class);
        ptr(class).cast()
    }

    /// A compiled class pair nothing has realized, named `name`.
    fn waiting_pair(name: &'static CStr) -> (*mut Class, *mut Class) {
        crate::bootstrap();
        crate::api::set_compiled_class_realizer(adopt_compiled);
        let meta = compiled_header(compiled_ro(RO_META));
        let class = compiled_header(compiled_ro(0));
        // SAFETY: a leaked header nothing else holds.
        unsafe { (*class).isa.store(meta, Ordering::Release) };
        COMPILED_NAMES.lock().unwrap().push((class as usize, name));
        COMPILED_METAS.lock().unwrap().push(meta as usize);
        (class, meta)
    }

    #[test]
    fn a_compiled_class_is_realized_on_first_touch() {
        let (class, meta) = waiting_pair(c"RosemaryRealizedOnTouch");
        assert!(!is_known(class));
        // SAFETY: a compiled header.
        let realized = unsafe { from_ptr(class) }.expect("realized");
        assert!(std::ptr::eq(realized, class));
        assert!(std::ptr::eq(realized.metaclass(), meta));
        assert!(lookup(c"RosemaryRealizedOnTouch").is_some());
    }

    #[test]
    fn an_object_whose_isa_is_a_waiting_metaclass_realizes_as_its_class() {
        let (class, meta) = waiting_pair(c"RosemaryRealizedThroughMeta");
        // A class is an object whose `isa` is its metaclass.
        // SAFETY: a compiled header, readable as an object.
        let found = unsafe { crate::object::class_of(class.cast()) }.expect("realized");
        assert!(std::ptr::eq(found, meta));
        assert!(is_known(class));
    }

    #[test]
    fn a_disposed_adopted_class_gets_its_compiled_header_back() {
        let (header, meta, class) = adopt_under_nsobject(c"RosemaryDisposedClass", COMPILED_RO);
        dispose_pair(class);

        for h in [header, meta] {
            // SAFETY: the leaked headers made above.
            let h = unsafe { &*h };
            assert!(h.isa.load(Ordering::Acquire).is_null());
            assert!(h.superclass.load(Ordering::Acquire).is_null());
            assert_eq!(h.bits(), COMPILED_RO);
        }
        assert!(!is_known(header) && !is_known(meta));
        assert!(lookup(c"RosemaryDisposedClass").is_none());
    }
}
