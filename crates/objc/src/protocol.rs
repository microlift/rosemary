use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char};
use std::sync::{LazyLock, RwLock};

use crate::property::Property;
use crate::sel::Sel;

/// One declared method: what to send, and what its signature is.
///
/// `#[repr(C)]` and returned by value, because `protocol_getMethodDescription`
/// returns a two-word struct and the caller reads it out of the registers the C
/// ABI puts it in.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Description {
    pub sel: Sel,
    pub types: *const c_char,
}

/// A protocol.
pub struct Protocol {
    name: CString,
    /// Keyed by `(selector, required, instance)`, the three coordinates
    /// `protocol_getMethodDescription` is asked for.
    contents: RwLock<Contents>,
    registered: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct Contents {
    methods: HashMap<(usize, bool, bool), &'static CStr>,
    adopted: Vec<&'static Protocol>,
    /// Required properties only, as Apple keeps them: instance ones, then
    /// class ones.
    instance_properties: Vec<&'static Property>,
    class_properties: Vec<&'static Property>,
}

// SAFETY: `name` is immutable, the rest is behind a lock or an atomic, and the
// pointers are to leaked values that outlive the process.
unsafe impl Sync for Protocol {}
unsafe impl Send for Protocol {}

impl Protocol {
    pub fn is_registered(&self) -> bool {
        self.registered.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn name(&self) -> *const c_char {
        self.name.as_ptr()
    }

    pub fn add_method(&self, sel: Sel, types: &CStr, required: bool, instance: bool) {
        crate::sel::validate(sel);
        let types: &'static CStr = Box::leak(CString::from(types).into_boxed_c_str());
        self.contents
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .methods
            .insert((sel as usize, required, instance), types);
    }

    pub fn add_protocol(&self, other: &'static Protocol) {
        if crate::debug::enabled() && (std::ptr::eq(self, other) || other.conforms_to(self)) {
            crate::fail!(
                "objc debug: adding {} to {} would make a protocol cycle",
                other.name.to_string_lossy(),
                self.name.to_string_lossy()
            );
        }
        self.contents
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .adopted
            .push(other);
    }

    /// The description of `sel`, or one with two nulls if this protocol has none.
    ///
    /// Two nulls rather than an absence, because that is the shape
    /// `protocol_getMethodDescription` returns by value.
    pub fn method(&self, sel: Sel, required: bool, instance: bool) -> Description {
        let contents = self.contents.read().unwrap_or_else(|e| e.into_inner());
        match contents.methods.get(&(sel as usize, required, instance)) {
            Some(types) => Description {
                sel,
                types: types.as_ptr(),
            },
            None => Description {
                sel: std::ptr::null(),
                types: std::ptr::null(),
            },
        }
    }

    pub fn name_c(&self) -> &CStr {
        &self.name
    }

    /// The protocols this one adopts directly.
    pub fn adopted(&self) -> Vec<&'static Protocol> {
        self.contents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .adopted
            .clone()
    }

    /// Declare a property. Only while the protocol is being built, and only a
    /// required one is kept: Apple's runtime has nowhere to put an optional one.
    pub fn add_property(&self, name: &CStr, attributes: &CStr, required: bool, instance: bool) {
        if self.is_registered() || !required {
            return;
        }
        let property = crate::property::make(name, attributes);
        let mut contents = self.contents.write().unwrap_or_else(|e| e.into_inner());
        match instance {
            true => contents.instance_properties.push(property),
            false => contents.class_properties.push(property),
        }
    }

    /// The methods this protocol declares itself in one of the four groups.
    pub fn methods(&self, required: bool, instance: bool) -> Vec<Description> {
        self.contents
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .methods
            .iter()
            .filter(|((_, r, i), _)| *r == required && *i == instance)
            .map(|((sel, _, _), types)| Description {
                sel: *sel as Sel,
                types: types.as_ptr(),
            })
            .collect()
    }

    /// The required properties this protocol declares itself.
    pub fn properties(&self, instance: bool) -> Vec<&'static Property> {
        let contents = self.contents.read().unwrap_or_else(|e| e.into_inner());
        match instance {
            true => contents.instance_properties.clone(),
            false => contents.class_properties.clone(),
        }
    }

    /// The required property `name`, declared here or by an adopted protocol.
    pub fn property(&self, name: &CStr, instance: bool) -> Option<&'static Property> {
        self.properties(instance)
            .into_iter()
            .find(|p| p.name_c() == name)
            .or_else(|| {
                self.adopted()
                    .into_iter()
                    .find_map(|p| p.property(name, instance))
            })
    }

    /// `protocol_conformsToProtocol`: the same name, or adopted directly or
    /// through another. By name, as Apple's is, because two images may each
    /// carry a copy of one protocol.
    pub fn conforms_by_name(&self, other: &Protocol) -> bool {
        self.name == other.name
            || self
                .adopted()
                .into_iter()
                .any(|p| p.conforms_by_name(other))
    }

    /// Whether this protocol adopts `other`, directly or through another.
    pub fn conforms_to(&self, other: &Protocol) -> bool {
        let contents = self.contents.read().unwrap_or_else(|e| e.into_inner());
        contents
            .adopted
            .iter()
            .any(|p| std::ptr::eq(*p, other) || p.conforms_to(other))
    }
}

static PROTOCOLS: LazyLock<RwLock<HashMap<String, &'static Protocol>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// A protocol by name, or `None`.
pub fn lookup(name: &CStr) -> Option<&'static Protocol> {
    let name = name.to_str().ok()?;
    PROTOCOLS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(name)
        .copied()
}

/// Every registered protocol, in no particular order.
pub fn registered() -> Vec<&'static Protocol> {
    PROTOCOLS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .copied()
        .collect()
}

/// Allocate an unregistered protocol, or `None` if the name is taken.
pub fn allocate(name: &CStr) -> Option<&'static Protocol> {
    if lookup(name).is_some() {
        return None;
    }
    let protocol = Box::leak(Box::new(Protocol {
        name: CString::from(name),
        contents: RwLock::new(Contents::default()),
        registered: std::sync::atomic::AtomicBool::new(false),
    }));
    crate::debug::note_handle(protocol, "protocol");
    Some(protocol)
}

/// Make `protocol` findable by name.
pub fn register(protocol: &'static Protocol) {
    if crate::debug::enabled() && protocol.is_registered() {
        crate::fail!(
            "objc debug: objc_registerProtocol called twice for {}",
            protocol.name.to_string_lossy()
        );
    }
    if protocol
        .registered
        .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return;
    }
    let name = protocol.name.to_string_lossy().into_owned();
    PROTOCOLS
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(name, protocol);
}
