use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
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
}

/// A class.
///
/// `#[repr(C)]` for the first two fields only: an object's `isa` and a class's
/// `superclass` are read by hand often enough that their offsets are ABI.
#[repr(C)]
pub struct Class {
    /// The metaclass. A class is an object, and this is its `isa`.
    isa: AtomicPtr<Class>,
    superclass: AtomicPtr<Class>,
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
    contents: RwLock<Contents>,
}

// SAFETY: every field is either immutable after construction or behind a lock or
// an atomic. The raw pointers are to leaked `Class`es, which outlive the process.
unsafe impl Sync for Class {}
unsafe impl Send for Class {}

/// Bumped whenever a method resolution anywhere could have changed.
///
/// A caller that caches a lookup compares this; see [`generation`].
static GENERATION: AtomicUsize = AtomicUsize::new(1);

fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
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
const CLASS_STRIDE: usize = size_of::<Class>();
const NO_ARENA: usize = usize::MAX;

static ARENA: AtomicUsize = AtomicUsize::new(0);
static BUILT: AtomicUsize = AtomicUsize::new(0);
static BUILDING: Mutex<()> = Mutex::new(());
static OVERFLOWED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Classes built past the arena. Sharded like the refcounts, because retain and
/// release ask [`is_known`] of every object once this is in use.
static OVERFLOW: [RwLock<crate::address::AddressMap<()>>; OVERFLOW_SHARDS] =
    [const { RwLock::new(HashMap::with_hasher(std::hash::BuildHasherDefault::new())) };
        OVERFLOW_SHARDS];
const OVERFLOW_SHARDS: usize = 64;

fn overflow(p: usize) -> &'static RwLock<crate::address::AddressMap<()>> {
    &OVERFLOW[(crate::address::mix(p as u64) >> 58) as usize % OVERFLOW_SHARDS]
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
    OVERFLOWED.load(Ordering::Acquire)
        && overflow(p as usize)
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&(p as usize))
}

fn arena_slot() -> Option<*mut Class> {
    if ARENA.load(Ordering::Relaxed) == 0 {
        // SAFETY: a fresh private anonymous reservation.
        let mapped = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                CLASS_CAPACITY * CLASS_STRIDE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        let base = match mapped == libc::MAP_FAILED {
            true => NO_ARENA,
            false => mapped as usize,
        };
        ARENA.store(base, Ordering::Relaxed);
    }
    let base = ARENA.load(Ordering::Relaxed);
    let index = BUILT.load(Ordering::Relaxed);
    (base != NO_ARENA && index < CLASS_CAPACITY)
        .then(|| (base + index * CLASS_STRIDE) as *mut Class)
}

impl Class {
    fn new(
        name: CString,
        superclass: *mut Class,
        is_meta: bool,
        instance_size: usize,
    ) -> &'static Class {
        let class = Class {
            isa: AtomicPtr::new(std::ptr::null_mut()),
            superclass: AtomicPtr::new(superclass),
            name,
            instance_size: AtomicUsize::new(instance_size),
            is_meta,
            registered: AtomicUsize::new(0),
            initialized: AtomicBool::new(false),
            instances: AtomicPtr::new(std::ptr::null_mut()),
            contents: RwLock::new(Contents::default()),
        };
        let _building = BUILDING.lock().unwrap_or_else(|e| e.into_inner());
        match arena_slot() {
            Some(slot) => {
                // SAFETY: an unused, aligned slot of a reservation that is never
                // unmapped, published only once written.
                unsafe { slot.write(class) };
                BUILT.fetch_add(1, Ordering::Release);
                // SAFETY: as above.
                unsafe { &*slot }
            }
            None => {
                let class: &'static Class = Box::leak(Box::new(class));
                overflow(std::ptr::from_ref(class) as usize)
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(std::ptr::from_ref(class) as usize, ());
                OVERFLOWED.store(true, Ordering::Release);
                class
            }
        }
    }

    pub fn name(&self) -> *const c_char {
        self.name.as_ptr()
    }

    pub fn name_str(&self) -> &str {
        self.name.to_str().unwrap_or("<non-utf8 class name>")
    }

    pub fn is_meta(&self) -> bool {
        self.is_meta
    }

    pub fn is_registered(&self) -> bool {
        self.registered.load(Ordering::Acquire) != 0
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
        self.instance_size.load(Ordering::Acquire)
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
        self.contents
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
        self.contents
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
        let mut contents = self.contents.write().unwrap_or_else(|e| e.into_inner());
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
        if self.is_registered() || self.is_meta {
            return false;
        }
        let mut contents = self.contents.write().unwrap_or_else(|e| e.into_inner());
        if contents.ivars.iter().any(|i| i.name.as_c_str() == name) {
            return false;
        }
        // `alignment` is log2, as `class_addIvar` documents. Apple caps it too.
        let align = 1usize << alignment.min(15);
        let start = self.instance_size.load(Ordering::Acquire);
        let offset = start.next_multiple_of(align);
        let ivar: &'static Ivar = Box::leak(Box::new(Ivar {
            name: CString::from(name),
            offset,
            size,
            types: CString::from(types),
        }));
        crate::debug::note_handle(ivar, "ivar");
        contents.ivars.push(ivar);
        self.instance_size.store(offset + size, Ordering::Release);
        true
    }

    /// The ivar this class or a superclass declares under `name`.
    pub fn ivar(&'static self, name: &CStr) -> Option<&'static Ivar> {
        self.chain().find_map(|c| {
            c.contents
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
        let mut contents = self.contents.write().unwrap_or_else(|e| e.into_inner());
        let property = crate::property::make(name, attributes);
        match contents.properties.iter().position(|p| p.name_c() == name) {
            Some(at) => contents.properties[at] = property,
            None => contents.properties.push(property),
        }
    }

    /// The properties this class declares, not counting a superclass's - which
    /// is what `class_copyPropertyList` answers.
    pub fn properties(&self) -> Vec<&'static crate::property::Property> {
        self.contents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .properties
            .clone()
    }

    /// The property this class or a superclass declares under `name`.
    pub fn property(&'static self, name: &CStr) -> Option<&'static crate::property::Property> {
        self.chain().find_map(|c| {
            c.contents
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .properties
                .iter()
                .find(|p| p.name_c() == name)
                .copied()
        })
    }

    pub fn add_protocol(&self, protocol: &'static crate::protocol::Protocol) -> bool {
        let mut contents = self.contents.write().unwrap_or_else(|e| e.into_inner());
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

    /// Whether this class or a superclass adopts `protocol`.
    pub fn conforms_to(&'static self, protocol: &crate::protocol::Protocol) -> bool {
        self.chain().any(|c| {
            c.contents
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
pub fn ensure_initialized(class: &'static Class) {
    if class.initialized.load(Ordering::Acquire) {
        return;
    }
    let class = match class.is_meta() {
        true => {
            let instances = class.instances.load(Ordering::Acquire);
            match instances.is_null() {
                true => return,
                // SAFETY: a leaked class, set when the pair was allocated.
                false => unsafe { &*(instances as *const Class) },
            }
        }
        false => class,
    };
    if class.initialized.load(Ordering::Acquire) || !class.is_registered() {
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
            if class.initialized.load(Ordering::Acquire) {
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
    class.initialized.store(true, Ordering::Release);
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
        && class.name.as_c_str() == name
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

static NAMED: AtomicUsize = AtomicUsize::new(1);

const LOOKUP_SLOTS: usize = 128;

thread_local! {
    static RECENT_LOOKUPS: [std::cell::Cell<(usize, usize, Option<&'static Class>)>; LOOKUP_SLOTS] =
        const { [const { std::cell::Cell::new((0, 0, None)) }; LOOKUP_SLOTS] };
}

/// Every registered class, by name.
static CLASSES: LazyLock<RwLock<HashMap<String, &'static Class>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

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
    let _guard = ALLOCATION.lock().unwrap_or_else(|e| e.into_inner());
    if lookup(name).is_some() {
        return None;
    }
    let name = CString::from(name);

    // An instance starts where the superclass's ended - at `isa` for a root.
    let instance_start = superclass.map_or(size_of::<usize>(), |s| s.instance_size());
    let class = Class::new(
        name.clone(),
        superclass.map_or(std::ptr::null_mut(), ptr),
        false,
        instance_start + extra,
    );

    // The metaclass's superclass is the superclass's metaclass, and a root
    // metaclass's superclass is the root class itself - which is what makes
    // `+alloc` findable from a metaclass.
    let meta_super = match superclass {
        Some(s) => ptr(s.metaclass()),
        None => ptr(class),
    };
    let meta = Class::new(name, meta_super, true, size_of::<usize>());

    // Every metaclass's `isa` is the *root* metaclass, and the root metaclass's
    // is itself. That is what stops `object_getClass` recursing forever.
    let root_meta = match superclass {
        Some(s) => root_metaclass(s),
        None => meta,
    };
    meta.isa.store(ptr(root_meta), Ordering::Release);
    meta.instances.store(ptr(class), Ordering::Release);
    class.isa.store(ptr(meta), Ordering::Release);
    Some(class)
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
    if class.registered.swap(1, Ordering::AcqRel) != 0 {
        return;
    }
    class.metaclass().registered.store(1, Ordering::Release);
    with_classes(|classes| {
        classes.insert(class.name_str().to_string(), class);
    });
}

/// Forget a class pair that was allocated and never registered.
pub fn dispose_pair(class: &'static Class) {
    if crate::debug::enabled() && class.is_registered() {
        crate::fail!(
            "objc debug: objc_disposeClassPair called for registered class {}",
            class.name_str()
        );
    }
    if class.is_registered() {
        with_classes(|classes| {
            classes.remove(class.name_str());
        });
    }
    // The `Class` itself stays leaked: a pointer to it may have escaped, and a
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
/// # Safety
///
/// `p` must be null or a pointer this crate produced.
pub unsafe fn from_ptr(p: *const Class) -> Option<&'static Class> {
    if p.is_null() {
        return None;
    }
    if crate::debug::enabled() && !is_known(p) {
        crate::fail!("objc debug: invalid class handle {p:p}");
    }
    // SAFETY: the caller's obligation, strengthened by the debug registry.
    Some(unsafe { &*p })
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
