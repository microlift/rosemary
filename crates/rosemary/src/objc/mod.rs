pub mod cxxivars;
pub mod dispatch;
pub mod encoding;
pub mod fast;
pub mod host;
/// Only where there is a host runtime for a host-architecture image to be given to.
#[cfg(target_os = "macos")]
pub mod native;
pub mod send;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicPtr, Ordering};

use machish::extensions::objc::{ObjcCategory, ObjcClass, ObjcImage, ObjcMethod, ObjcProtocol};
use machish::report::LinkReport;

use crate::abi::GuestMemory;
use crate::program::AddrMap;
use crate::vm::VmError;

/// What realizing a guest's Objective-C metadata needs from the backend running it.
///
/// The two backends differ in one thing, and it runs through every pass below:
/// whether anything stands between the guest and the runtime. An emulated
/// guest's sends are intercepted, so its methods are reached through thunks and
/// a class is named by the guest's own `objc_class`. A native image calls
/// `objc_msgSend` for real, so its `IMP`s are host functions already and every
/// class it is handed has to be the registered one.
pub trait Realizer {
    fn report(&self) -> &LinkReport;

    /// The program's memory, for the two passes that write back into the
    /// guest's own metadata.
    fn memory(&self) -> Box<dyn GuestMemory + '_>;

    fn is_guest_address(&self, va: u64) -> bool;

    /// Call guest code - a `+load`, and nothing else here.
    fn call(&self, f: u64, args: &[u64]) -> Result<u64, VmError>;

    /// The `IMP` to install for `m`, and what about the method could not be
    /// carried faithfully to a host caller.
    fn imp_for(&self, m: &ObjcMethod) -> Result<(u64, Option<&'static str>), &'static str>;

    /// What guest code is handed for a class this pass registered.
    fn class_value(&self, rt: &ObjcRuntime, guest_va: u64) -> u64;
}

/// Something the guest declared that could not be reproduced faithfully.
///
/// Each is a named stop with the guest's own name in it, in the register the rest of
/// this project uses for refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The host runtime already has a class of this name.
    NameTaken { name: String },
    /// The superclass is neither a host class nor a guest class this pass registered -
    /// an unsatisfied weak import, or a class whose own registration was refused.
    SuperclassMissing { name: String, superclass: u64 },
    /// A superclass chain that comes back to itself. Not producible by a
    /// compiler; guarded because this walks a graph read out of guest memory.
    SuperclassCycle { name: String },
    /// `objc_allocateClassPair` declined, which it does for a name already in
    /// use and little else.
    AllocationFailed { name: String },
    /// An ivar the runtime would not add. Refusing the class rather than the ivar is
    /// deliberate.
    IvarRejected { name: String, ivar: String },
    /// A method that is installed and works for a *guest* sender, but which the
    /// **host** cannot call faithfully.
    MethodNotHostCallable {
        name: String,
        method: String,
        reason: &'static str,
    },
    /// A class with C++ ivars whose constructors could not be arranged for.
    CxxIvars {
        name: String,
        reason: cxxivars::Refusal,
    },
    /// A method whose `IMP` could not be made callable from the host.
    MethodRejected {
        name: String,
        method: String,
        reason: &'static str,
    },
    /// A Swift class, identified by the low bits of its `bits` word. Its
    /// metadata is not an `objc_class`.
    SwiftClass { name: String },
    /// A category on a class that is neither the host's nor one registered
    /// here.
    CategoryTargetMissing { name: String, class: u64 },
    /// A name with an interior NUL, which cannot round-trip through the C API.
    UnrepresentableName { name: String },
}

/// What one registration pass did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub classes: usize,
    pub methods: usize,
    pub ivars: usize,
    pub properties: usize,
    pub protocols: usize,
    pub categories: usize,
    pub selectors: usize,
    /// Ivar offset words rewritten with the runtime's answer.
    pub offsets_patched: usize,
}

/// The guest's classes, as the host runtime now knows them.
#[derive(Clone, Debug)]
pub struct ObjcRuntime {
    /// Guest `objc_class` VA -> the host `Class` standing for it.
    ///
    /// Address-hashed, like the other two: these are asked for pointers crossing
    /// on every message, where SipHash showed up on a profile by itself.
    to_host: AddrMap<u64>,
    /// The reverse, so a `Class` coming *back* from the host runtime can be handed to
    /// the guest as the address the guest already has.
    to_guest: AddrMap<u64>,
    /// Guest `protocol_t` VA -> host `Protocol *`.
    protocols: AddrMap<u64>,
    /// The half-open bounds of `to_host`'s keys, and of `to_guest`'s.
    ///
    /// Every pointer crossing the seam asks one of these maps, and almost every
    /// one of them is an ordinary object that is in neither. Two compares
    /// answer that without hashing.
    guest_bounds: (u64, u64),
    host_bounds: (u64, u64),
    /// The same, over `protocols`' keys. A message's `@` argument that is not a
    /// class is asked of the protocols next, and is almost never one either.
    protocol_bounds: (u64, u64),
    stats: Stats,
    refusals: Vec<Refusal>,
}

impl Default for ObjcRuntime {
    fn default() -> Self {
        Self {
            to_host: AddrMap::default(),
            to_guest: AddrMap::default(),
            protocols: AddrMap::default(),
            // Empty, so every range test fails.
            guest_bounds: (u64::MAX, 0),
            host_bounds: (u64::MAX, 0),
            protocol_bounds: (u64::MAX, 0),
            stats: Stats::default(),
            refusals: Vec::new(),
        }
    }
}

/// The runtime the process has published, for a caller with no [`Vm`] in hand.
static PUBLISHED: AtomicPtr<ObjcRuntime> = AtomicPtr::new(core::ptr::null_mut());

/// Record `rt` as the runtime a context-free caller should ask.
///
/// Called by [`Vm::publish_objc`] with the same leaked reference it stores in
/// the VM, so the two never disagree about which snapshot is current.
pub(crate) fn publish(rt: &'static ObjcRuntime) {
    PUBLISHED.store(rt as *const _ as *mut ObjcRuntime, Ordering::Release);
}

/// The published runtime, or `None` before any registration pass has run.
pub fn published() -> Option<&'static ObjcRuntime> {
    let rt = PUBLISHED.load(Ordering::Acquire);
    // SAFETY: non-null only after `publish` stored a leaked reference, which is
    // never freed and never mutated after publication - an extending pass
    // builds a separate copy and publishes that. Same argument as `Vm::objc`.
    (!rt.is_null()).then(|| unsafe { &*rt })
}

impl ObjcRuntime {
    /// The host `Class` for a guest `objc_class` address, if it was registered.
    pub fn host_class(&self, guest_va: u64) -> Option<u64> {
        self.to_host.get(&guest_va).copied()
    }

    /// The guest `objc_class` address a host `Class` stands for, if any.
    pub fn guest_class(&self, host: u64) -> Option<u64> {
        self.to_guest.get(&host).copied()
    }

    /// [`ObjcRuntime::host_class`], leaving anything that is not a guest class alone.
    ///
    /// The form the seam actually wants.
    pub fn host_or(&self, va: u64) -> u64 {
        if va < self.guest_bounds.0 || va >= self.guest_bounds.1 {
            return va;
        }
        self.host_class(va).unwrap_or(va)
    }

    /// [`ObjcRuntime::guest_class`], with the same rule for the other direction.
    pub fn guest_or(&self, host: u64) -> u64 {
        if host < self.host_bounds.0 || host >= self.host_bounds.1 {
            return host;
        }
        self.guest_class(host).unwrap_or(host)
    }

    /// Record that `host` stands for the guest class at `guest_va`, both ways.
    fn link(&mut self, guest_va: u64, host: u64) {
        self.to_host.insert(guest_va, host);
        self.to_guest.insert(host, guest_va);
        self.guest_bounds.0 = self.guest_bounds.0.min(guest_va);
        self.guest_bounds.1 = self.guest_bounds.1.max(guest_va + 1);
        self.host_bounds.0 = self.host_bounds.0.min(host);
        self.host_bounds.1 = self.host_bounds.1.max(host + 1);
    }

    /// The host `Protocol *` for a guest `protocol_t` address, leaving anything that is
    /// not one of this pass's protocols alone.
    pub fn host_protocol_or(&self, va: u64) -> u64 {
        if va < self.protocol_bounds.0 || va >= self.protocol_bounds.1 {
            return va;
        }
        self.protocols.get(&va).copied().unwrap_or(va)
    }

    /// Record that `host` is the `Protocol *` for the guest's at `guest_va`.
    fn link_protocol(&mut self, guest_va: u64, host: u64) {
        self.protocols.insert(guest_va, host);
        self.protocol_bounds.0 = self.protocol_bounds.0.min(guest_va);
        self.protocol_bounds.1 = self.protocol_bounds.1.max(guest_va + 1);
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn refusals(&self) -> &[Refusal] {
        &self.refusals
    }

    /// Whether every class the guest declared was registered.
    pub fn is_complete(&self) -> bool {
        self.refusals.is_empty()
    }
}

/// Register every image's Objective-C metadata with the host runtime.
pub fn register(r: &dyn Realizer) -> Result<ObjcRuntime, VmError> {
    let mut rt = ObjcRuntime::default();
    register_images(r, &mut rt, &images_of(r.report()))?;
    crate::objcexc::repair_image_records(&mut *r.memory(), r.report());
    Ok(rt)
}

/// Realize the metadata of an image the *guest* loaded, into an existing runtime.
///
/// The same five passes [`register`] runs, over a `dlopen`ed image's report instead of
/// the program's.
pub(crate) fn register_loaded(
    r: &dyn Realizer,
    rt: &mut ObjcRuntime,
    report: &LinkReport,
) -> Result<(), VmError> {
    register_images(r, rt, &images_of(report))?;
    crate::objcexc::repair_image_records(&mut *r.memory(), report);
    Ok(())
}

/// Every image in `report` carrying Objective-C metadata, dependencies first.
fn images_of(report: &LinkReport) -> Vec<&ObjcImage> {
    report
        .images
        .iter()
        .rev()
        .filter_map(|i| i.objc.as_ref())
        .collect()
}

/// The five passes, in the order the runtime needs them.
fn register_images(
    r: &dyn Realizer,
    rt: &mut ObjcRuntime,
    images: &[&ObjcImage],
) -> Result<(), VmError> {
    // Protocols first - a class adopts them as it is registered.
    for image in images {
        for p in &image.protocols {
            register_protocol(rt, p);
        }
    }

    // Then classes, superclass-first. The index is built across every image so
    // a subclass can find a superclass declared in another one.
    let by_va: HashMap<u64, &ObjcClass> = images
        .iter()
        .flat_map(|i| i.classes.iter())
        .map(|c| (c.va, c))
        .collect();
    for image in images {
        for c in &image.classes {
            let mut chain = HashSet::new();
            register_class(r, rt, &by_va, c, &mut chain);
        }
    }

    // Ivar offsets, once every class is realized and the runtime has laid them
    // out.
    for image in images {
        for c in &image.classes {
            patch_ivar_offsets(r, rt, c)?;
        }
    }

    // Categories last: one may extend a class registered above.
    for image in images {
        for c in &image.categories {
            register_category(r, rt, c);
        }
    }

    // Selector uniquing, which is independent of all of the above: it rewrites
    // the guest's own `__objc_selrefs` slots from string pointers to `SEL`s.
    for image in images {
        rt.stats.selectors += unique_selectors(r, image)?;
    }

    Ok(())
}

/// Call every `+load` the guest declares, superclass first.
pub fn call_load_methods(r: &dyn Realizer, rt: &ObjcRuntime) -> Result<usize, VmError> {
    load_images(r, rt, &images_of(r.report()))
}

/// Every `+load` in an image the guest loaded.
pub(crate) fn call_loaded_load_methods(
    r: &dyn Realizer,
    rt: &ObjcRuntime,
    report: &LinkReport,
) -> Result<usize, VmError> {
    load_images(r, rt, &images_of(report))
}

fn load_images(
    r: &dyn Realizer,
    rt: &ObjcRuntime,
    images: &[&ObjcImage],
) -> Result<usize, VmError> {
    let by_va: HashMap<u64, &ObjcClass> = images
        .iter()
        .flat_map(|i| i.classes.iter())
        .map(|c| (c.va, c))
        .collect();

    let mut done = HashSet::new();
    let mut called = 0;
    for image in images {
        for class in &image.classes {
            called += load_class(r, rt, &by_va, class, &mut done)?;
        }
    }
    // A category's `+load` is a method on the target class's metaclass and is
    // called even when the class itself has one - both run, which is the one
    // place Objective-C does not collapse a category over a class.
    for image in images {
        for cat in &image.categories {
            for m in &cat.class_methods {
                if m.name == "load" && r.is_guest_address(m.imp) {
                    let target = rt.host_or(cat.class);
                    r.call(m.imp, &[target, load_selector()])?;
                    called += 1;
                }
            }
        }
    }
    Ok(called)
}

/// One class's `+load`, its superclass's first.
fn load_class(
    r: &dyn Realizer,
    rt: &ObjcRuntime,
    by_va: &HashMap<u64, &ObjcClass>,
    class: &ObjcClass,
    done: &mut HashSet<u64>,
) -> Result<usize, VmError> {
    if !done.insert(class.va) {
        return Ok(0);
    }
    let mut called = 0;
    if let Some(parent) = by_va.get(&class.superclass).copied() {
        called += load_class(r, rt, by_va, parent, done)?;
    }
    // Only a class this pass registered.
    if rt.host_class(class.va).is_none() {
        return Ok(called);
    }
    let Some(meta) = &class.meta else {
        return Ok(called);
    };
    for m in &meta.ro.methods {
        if m.name == "load" && r.is_guest_address(m.imp) {
            r.call(m.imp, &[r.class_value(rt, class.va), load_selector()])?;
            called += 1;
        }
    }
    Ok(called)
}

/// The uniqued `load` selector, as the second argument every method takes.
fn load_selector() -> u64 {
    // SAFETY: a NUL-terminated literal; the returned `SEL` is interned by the
    // runtime for the process lifetime.
    unsafe { host::sel_registerName(c"load".as_ptr()) as u64 }
}

/// A failed write into the guest's own metadata, named with the address.
///
/// Distinct from every other error here.
fn memory_error(addr: u64) -> VmError {
    VmError::Abi(crate::abi::AbiError::Memory {
        symbol: "objc metadata".to_string(),
        addr,
    })
}

/// Create one class, registering its superclass first if that is also a guest's.
fn register_class(
    r: &dyn Realizer,
    rt: &mut ObjcRuntime,
    by_va: &HashMap<u64, &ObjcClass>,
    class: &ObjcClass,
    chain: &mut HashSet<u64>,
) -> Option<u64> {
    if let Some(&already) = rt.to_host.get(&class.va) {
        return Some(already);
    }
    if !chain.insert(class.va) {
        rt.refusals.push(Refusal::SuperclassCycle {
            name: class.ro.name.clone(),
        });
        return None;
    }

    let name = class.ro.name.clone();

    // A Swift class's `bits` carries one of the two Swift markers, and what it
    // points at is Swift metadata rather than a `class_ro_t`.
    if class.is_swift() {
        rt.refusals.push(Refusal::SwiftClass { name });
        return None;
    }

    let Some(c_name) = host::c_string(&name) else {
        rt.refusals.push(Refusal::UnrepresentableName { name });
        return None;
    };

    // The superclass is a bind, so it is either a host `Class` already or the
    // address of another guest class this pass has to have created first.
    let superclass = if class.ro.is_root() {
        std::ptr::null_mut()
    } else if let Some(&host_super) = rt.to_host.get(&class.superclass) {
        host_super as host::Class
    } else if let Some(guest_super) = by_va.get(&class.superclass).copied() {
        match register_class(r, rt, by_va, guest_super, chain) {
            Some(c) => c as host::Class,
            None => {
                rt.refusals.push(Refusal::SuperclassMissing {
                    name,
                    superclass: class.superclass,
                });
                return None;
            }
        }
    } else if class.superclass != 0 && !r.is_guest_address(class.superclass) {
        // A host class, resolved by the ordinary native passthrough. This is
        // the common case: `Greeter : NSObject` binds `_OBJC_CLASS_$_NSObject`
        // to the host's own, which is exactly the class we want to derive from.
        class.superclass as host::Class
    } else {
        rt.refusals.push(Refusal::SuperclassMissing {
            name,
            superclass: class.superclass,
        });
        return None;
    };

    // SAFETY: `c_name` is NUL-terminated and outlives the call; `superclass` is
    // either null, a `Class` the host runtime gave us, or one it created above.
    let cls = unsafe { host::objc_allocateClassPair(superclass, c_name.as_ptr(), 0) };
    if cls.is_null() {
        // The runtime declines exactly one thing here, and it is worth telling
        // apart: a name already in use. A guest defining `NSString` is a
        // different problem from a guest defining something unrepresentable.
        // SAFETY: `c_name` as above.
        let taken = unsafe { !host::objc_lookUpClass(c_name.as_ptr()).is_null() };
        rt.refusals.push(if taken {
            Refusal::NameTaken { name }
        } else {
            Refusal::AllocationFailed { name }
        });
        return None;
    }

    // Ivars, before registration: the runtime lays the class out at
    // `objc_registerClassPair` and will not take one afterwards.
    let mut ivars = 0;
    for ivar in &class.ro.ivars {
        let (Some(n), Some(t)) = (host::c_string(&ivar.name), host::c_string(&ivar.types)) else {
            rt.refusals.push(Refusal::UnrepresentableName {
                name: format!("{name}.{}", ivar.name),
            });
            // SAFETY: `cls` was allocated above and not yet registered, which is
            // exactly when disposing is allowed.
            unsafe { host::objc_disposeClassPair(cls) };
            return None;
        };
        // `alignment` is already a log2 in the guest's `ivar_t`, which is what
        // `class_addIvar` wants.
        // SAFETY: strings outlive the call; `cls` is unregistered.
        let ok = unsafe {
            host::class_addIvar(
                cls,
                n.as_ptr(),
                ivar.size as usize,
                ivar.alignment.min(15) as u8,
                t.as_ptr(),
            )
        };
        if !ok {
            rt.refusals.push(Refusal::IvarRejected {
                name,
                ivar: ivar.name.clone(),
            });
            unsafe { host::objc_disposeClassPair(cls) };
            return None;
        }
        ivars += 1;
    }

    // Protocols the class adopts.
    for &p in &class.ro.protocols {
        if let Some(&proto) = rt.protocols.get(&p) {
            // SAFETY: `proto` is a `Protocol *` this pass registered.
            unsafe { host::class_addProtocol(cls, proto as host::Protocol) };
        }
    }

    // Instance methods on the class, class methods on its metaclass. A class
    // method added to the class itself is silently an instance method, which is
    // a wrong program with no diagnostic.
    // SAFETY: `cls` is a `Class`; `object_getClass` of one is its metaclass.
    let meta = unsafe { host::object_getClass(cls) };
    let no_class_methods: Vec<ObjcMethod> = Vec::new();
    let class_methods = class
        .meta
        .as_ref()
        .map(|m| &m.ro.methods)
        .unwrap_or(&no_class_methods);
    let mut methods = 0;
    // The `IMP` installed for `.cxx_construct`, if the class has one.
    let mut cxx_construct: Option<host::Imp> = None;
    for (target, list) in [(cls, &class.ro.methods), (meta, class_methods)] {
        for m in list {
            match add_method(r, target, m) {
                Ok(carried) => {
                    methods += 1;
                    if target == cls && m.name == cxxivars::CONSTRUCT {
                        // SAFETY: the method was just installed on `cls`, so
                        // asking the class for it answers what `add_method`
                        // wrote - including the typed trampoline, when there
                        // was one, which a saved copy of the raw thunk would
                        // miss.
                        cxx_construct = unsafe {
                            host::c_string(&m.name)
                                .map(|n| host::sel_registerName(n.as_ptr()))
                                .map(|sel| host::class_getMethodImplementation(cls, sel))
                                .filter(|imp| !imp.is_null())
                        };
                    }
                    if let Some(reason) = carried {
                        rt.refusals.push(Refusal::MethodNotHostCallable {
                            name: name.to_string(),
                            method: m.name.clone(),
                            reason,
                        });
                    }
                }
                Err(reason) => {
                    rt.refusals.push(Refusal::MethodRejected {
                        name,
                        method: m.name.clone(),
                        reason,
                    });
                    // SAFETY: as above - still unregistered.
                    unsafe { host::objc_disposeClassPair(cls) };
                    return None;
                }
            }
        }
    }

    // C++ ivars, before the pair is registered because this adds two methods.
    if let Some(ctor) = cxx_construct {
        // SAFETY: `cls` is a class this pass allocated and has not registered,
        // and `ctor` is the `IMP` it installed for `.cxx_construct` on it.
        for reason in unsafe { cxxivars::install(cls, ctor, &name) } {
            rt.refusals.push(Refusal::CxxIvars {
                name: name.to_string(),
                reason,
            });
        }
    }

    // SAFETY: `cls` was allocated by `objc_allocateClassPair` and is registered
    // exactly once; every later use is through the maps below.
    unsafe { host::objc_registerClassPair(cls) };

    let host_va = cls as u64;
    rt.link(class.va, host_va);
    // The metaclass is a class the guest names too.
    if let Some(declared) = &class.meta {
        rt.link(declared.va, meta as u64);
    }
    rt.stats.classes += 1;
    rt.stats.methods += methods;
    rt.stats.ivars += ivars;
    rt.stats.properties += add_properties(cls, &class.ro.properties)
        + class.meta.as_ref().map_or(0, |m| {
            // SAFETY: `cls` is a `Class`, so `object_getClass` of it is the
            // metaclass, which is where a class property lives.
            add_properties(unsafe { host::object_getClass(cls) }, &m.ro.properties)
        });
    chain.remove(&class.va);
    Some(host_va)
}

/// Install one guest method on `target`.
fn add_method(
    r: &dyn Realizer,
    target: host::Class,
    m: &ObjcMethod,
) -> Result<Option<&'static str>, &'static str> {
    let (Some(name), Some(types)) = (host::c_string(&m.name), host::c_string(&m.types)) else {
        return Err("the selector or type encoding has an interior NUL");
    };
    if !r.is_guest_address(m.imp) {
        return Err("the implementation is not a guest address");
    }
    let (imp, carried) = r.imp_for(m)?;

    // SAFETY: both strings outlive the call; `imp` is an executable arm64
    // trampoline this runtime built and never frees.
    let sel = unsafe { host::sel_registerName(name.as_ptr()) };
    let ok = unsafe { host::class_addMethod(target, sel, imp as host::Imp, types.as_ptr()) };
    // `false` means the class already has the selector, which for a freshly
    // allocated class means the guest declared it twice.
    if ok {
        Ok(carried)
    } else {
        Err("the class already responds to that selector")
    }
}

/// A method's own type encoding, as the signature a thunk into it needs.
///
/// `Err` is a `&'static str` in the register the rest of this project uses for
/// refusals - it names what about *this* method could not be carried, so the
/// listing says "returns an aggregate by value" rather than "unsupported".
///
/// - **An aggregate return too large for AAPCS64's registers.** The callee fills a
/// buffer through `x8`, and a trampoline has no way to declare that register.
pub(crate) fn thunk_signature(
    m: &ObjcMethod,
) -> Result<crate::thunk::ThunkSignature, &'static str> {
    let signature = encoding::signature(&m.types)?;
    let (returns, params) = signature
        .shapes
        .split_first()
        .ok_or("the type encoding describes no return value")?;
    // `Scalar(None)` is how the decoder spells `void`; a thunk returning
    // nothing declares no return rather than an ignored zero.
    let returns = match returns {
        crate::abi::aggregate::Shape::Scalar(crate::abi::ArgType::None) => None,
        other => Some(other.clone()),
    };
    crate::thunk::ThunkSignature::new(params.to_vec(), returns)
}

/// Recreate a protocol the guest declares.
fn register_protocol(rt: &mut ObjcRuntime, p: &ObjcProtocol) {
    let Some(name) = host::c_string(&p.name) else {
        rt.refusals.push(Refusal::UnrepresentableName {
            name: p.name.clone(),
        });
        return;
    };
    // SAFETY: `name` is NUL-terminated and outlives the call.
    let existing = unsafe { host::objc_getProtocol(name.as_ptr()) };
    if !existing.is_null() {
        rt.link_protocol(p.va, existing as u64);
        return;
    }
    let proto = unsafe { host::objc_allocateProtocol(name.as_ptr()) };
    if proto.is_null() {
        return;
    }

    for (list, required, instance) in [
        (&p.instance_methods, true, true),
        (&p.class_methods, true, false),
        (&p.optional_instance_methods, false, true),
        (&p.optional_class_methods, false, false),
    ] {
        for m in list {
            let (Some(n), Some(t)) = (host::c_string(&m.name), host::c_string(&m.types)) else {
                continue;
            };
            // SAFETY: strings outlive the call; `proto` is unregistered, which
            // is when descriptions may be added.
            unsafe {
                let sel = host::sel_registerName(n.as_ptr());
                host::protocol_addMethodDescription(proto, sel, t.as_ptr(), required, instance);
            }
        }
    }
    for &inc in &p.protocols {
        if let Some(&other) = rt.protocols.get(&inc) {
            // SAFETY: `other` is a `Protocol *` registered by this pass.
            unsafe { host::protocol_addProtocol(proto, other as host::Protocol) };
        }
    }

    // SAFETY: `proto` came from `objc_allocateProtocol` and is registered once.
    unsafe { host::objc_registerProtocol(proto) };
    rt.link_protocol(p.va, proto as u64);
    rt.stats.protocols += 1;
}

/// Attach a category's methods to its target class.
fn register_category(r: &dyn Realizer, rt: &mut ObjcRuntime, cat: &ObjcCategory) {
    let target = if let Some(&registered) = rt.to_host.get(&cat.class) {
        registered as host::Class
    } else if cat.class != 0 && !r.is_guest_address(cat.class) {
        cat.class as host::Class
    } else {
        rt.refusals.push(Refusal::CategoryTargetMissing {
            name: cat.name.clone(),
            class: cat.class,
        });
        return;
    };
    // SAFETY: `target` is a `Class` - either the host's own or one registered
    // above - so its metaclass is what `object_getClass` returns.
    let meta = unsafe { host::object_getClass(target) };

    let mut added = 0;
    for (on, list) in [(target, &cat.instance_methods), (meta, &cat.class_methods)] {
        for m in list {
            match add_method(r, on, m) {
                Ok(carried) => {
                    added += 1;
                    if let Some(reason) = carried {
                        rt.refusals.push(Refusal::MethodNotHostCallable {
                            name: cat.name.clone(),
                            method: m.name.clone(),
                            reason,
                        });
                    }
                }
                Err(reason) => rt.refusals.push(Refusal::MethodRejected {
                    name: cat.name.clone(),
                    method: m.name.clone(),
                    reason,
                }),
            }
        }
    }
    rt.stats.methods += added;
    rt.stats.properties += add_properties(target, &cat.instance_properties)
        + add_properties(meta, &cat.class_properties);
    rt.stats.categories += 1;
}

/// Replay a `property_list_t` onto a class.
///
/// Nothing reads a property to run the program: an accessor is an ordinary
/// method and was added with the rest. What reads one is code introspecting a
/// class it did not write, and to that a class with no properties and a class
/// this pass dropped the properties of look exactly alike.
///
/// The guest carries one attribute *string* per property and `class_addProperty`
/// takes the fields apart, so the split is here. Returns how many were added.
fn add_properties(
    cls: host::Class,
    properties: &[machish::extensions::objc::ObjcProperty],
) -> usize {
    let mut added = 0;
    for property in properties {
        let (Some(name), Some(attributes)) = (
            host::c_string(&property.name),
            host::c_string(&property.attributes),
        ) else {
            continue;
        };
        let fields = objc::property::attributes_of(&attributes);
        let entries: Vec<host::PropertyAttribute> = fields
            .iter()
            .map(|(n, v)| host::PropertyAttribute {
                name: n.as_ptr(),
                value: v.as_ptr(),
            })
            .collect();
        // SAFETY: `cls` is a `Class`, and the strings outlive the call - which
        // is all the runtime needs, because it copies them.
        added += unsafe {
            host::class_addProperty(cls, name.as_ptr(), entries.as_ptr(), entries.len() as u32)
        } as usize;
    }
    added
}

/// Write the runtime's ivar offsets back into the guest's own `__objc_ivar`.
fn patch_ivar_offsets(
    r: &dyn Realizer,
    rt: &mut ObjcRuntime,
    class: &ObjcClass,
) -> Result<(), VmError> {
    let Some(&cls) = rt.to_host.get(&class.va) else {
        return Ok(());
    };
    let mut view = r.memory();
    for ivar in &class.ro.ivars {
        if ivar.offset_va == 0 {
            continue;
        }
        let Some(name) = host::c_string(&ivar.name) else {
            continue;
        };
        // SAFETY: `cls` is a registered `Class`; `name` outlives the call.
        let found = unsafe { host::class_getInstanceVariable(cls as host::Class, name.as_ptr()) };
        if found.is_null() {
            continue;
        }
        // SAFETY: `found` is an `Ivar` the runtime just returned for `cls`.
        let offset = unsafe { host::ivar_getOffset(found) };

        // The guest's word is 32 bits - `int32_t`, per `ivar_t` - and a negative
        // or oversized offset would mean the runtime laid out something this
        // does not understand, so it is refused rather than truncated.
        let Ok(offset) = u32::try_from(offset) else {
            continue;
        };
        view.write(ivar.offset_va, &offset.to_le_bytes())
            .map_err(|_| memory_error(ivar.offset_va))?;
        rt.stats.offsets_patched += 1;
    }
    Ok(())
}

/// Point the guest's own class and protocol references at the registered objects.
///
/// libobjc does this when dyld maps an image. The emulated backend does not need
/// it - a send is intercepted, and `dispatch` translates the receiver on the way
/// through - but a native image reaches `objc_msgSend` directly, so a classref
/// still holding the guest's own `objc_class` is a message to a structure this
/// runtime never made.
/// `report` is which images to walk, and is not always `r`'s own: an image the
/// guest `dlopen`ed has a report of its own, and its references need this pass as
/// much as the executable's did.
pub(crate) fn rewrite_refs(
    r: &dyn Realizer,
    rt: &ObjcRuntime,
    report: &LinkReport,
) -> Result<usize, VmError> {
    let mut view = r.memory();
    let mut rewritten = 0;
    for image in report.images.iter().filter_map(|i| i.objc.as_ref()) {
        // A pre-10.6 compiler names its own classes in code, where no slot can
        // be rewritten; see `host::alias_class`.
        let declared = image
            .classes
            .iter()
            .flat_map(|c| core::iter::once(c.va).chain(c.meta.as_ref().map(|m| m.va)));
        for va in declared {
            if let Some(host) = rt.host_class(va) {
                host::alias_class(va, host);
            }
        }
        // A superref names the class a `[super ...]` send starts its search
        // from, and is a class reference in every other respect.
        let classes = image.class_refs.iter().chain(image.super_refs.iter());
        for (slot, translate) in classes
            .map(|s| (s, false))
            .chain(image.protocol_refs.iter().map(|s| (s, true)))
        {
            let mut buf = [0u8; 8];
            if view.read(*slot, &mut buf).is_err() {
                continue;
            }
            let va = u64::from_le_bytes(buf);
            if va == 0 {
                continue;
            }
            let host = match translate {
                true => rt.host_protocol_or(va),
                false => rt.host_or(va),
            };
            if host == va {
                continue;
            }
            view.write(*slot, &host.to_le_bytes())
                .map_err(|_| memory_error(*slot))?;
            rewritten += 1;
        }
    }
    Ok(rewritten)
}

/// Turn every `__objc_selrefs` slot from a string pointer into a `SEL`.
fn unique_selectors(r: &dyn Realizer, image: &ObjcImage) -> Result<usize, VmError> {
    let mut uniqued = 0;
    let mut view = r.memory();
    for &slot in image
        .selector_refs
        .iter()
        .chain(&image.message_ref_selectors)
    {
        let mut buf = [0u8; 8];
        if view.read(slot, &mut buf).is_err() {
            continue;
        }
        let name_va = u64::from_le_bytes(buf);
        if name_va == 0 {
            continue;
        }
        let Ok(bytes) = (&*view).read_cstr(name_va, "objc selector") else {
            continue;
        };
        let Ok(name) = std::ffi::CString::new(bytes) else {
            continue;
        };
        // SAFETY: `name` is NUL-terminated and outlives the call. The returned
        // `SEL` is interned by the runtime for the process lifetime.
        let sel = unsafe { host::sel_registerName(name.as_ptr()) };
        view.write(slot, &(sel as u64).to_le_bytes())
            .map_err(|_| memory_error(slot))?;
        uniqued += 1;
    }
    Ok(uniqued)
}

/// Unique the selector references of an image the *guest* loaded, and nothing else
/// about it.
///
/// The one pass of [`register_loaded`]'s five that has to happen even when the rest
/// cannot - when no `Vm` has registered anything yet, or when the image declares no
/// classes at all.
pub(crate) fn unique_loaded_selectors(
    r: &dyn Realizer,
    report: &LinkReport,
) -> Result<usize, VmError> {
    let mut uniqued = 0;
    for image in report.images.iter().filter_map(|i| i.objc.as_ref()) {
        uniqued += unique_selectors(r, image)?;
    }
    Ok(uniqued)
}
