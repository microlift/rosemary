//! Key-value observing.
//!
//! An observation of `a.b.c` on an object watches every `(object, key)` pair
//! whose change is a change of that path: `a` on the object, `b` on the value
//! of `a`, `c` on the value of `b`, and each key any of those depend on through
//! `+keyPathsForValuesAffectingValueForKey:`. A change to any of them notifies,
//! and the pairs past it are worked out again, since the objects there may be
//! different ones now.
//!
//! A watched object whose class has a setter for the key is moved to an
//! `NSKVONotifying_<Class>` subclass whose setter wraps the original in
//! `-willChangeValueForKey:` and `-didChangeValueForKey:`, which is how Apple
//! makes plain setters notify. The subclass answers `-class` with the original.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::sync::{LazyLock, Mutex, MutexGuard, RwLock};

use objc::class::{Class, Imp};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};
use crate::geometry::{NSPoint, NSRange, NSRect};
use crate::kvc::Shape;

/// `NSKeyValueObservingOptions`.
mod options {
    /// `NSKeyValueObservingOptionNew`: put the new value in the change dictionary.
    pub const NEW: usize = 0x01;
    /// `NSKeyValueObservingOptionOld`.
    pub const OLD: usize = 0x02;
    /// `NSKeyValueObservingOptionInitial`: notify once at registration, before any
    /// change.
    pub const INITIAL: usize = 0x04;
    /// `NSKeyValueObservingOptionPrior`: notify before the change as well.
    pub const PRIOR: usize = 0x08;
}

/// `NSKeyValueChangeKindKey`'s value for a plain assignment.
const CHANGE_SETTING: i64 = 1;

/// Paths and dependencies nest no deeper than this, so a key that depends on
/// itself through another stops rather than recursing forever.
const MAX_DEPTH: usize = 16;

/// One registration. Addresses rather than `Id`s so the table can be a `static`.
struct Observation {
    observed: usize,
    observer: usize,
    key_path: Vec<u8>,
    context: usize,
    options: usize,
    /// Every `(object, key)` whose change is a change of `key_path`.
    watches: Vec<(usize, Vec<u8>)>,
}

#[derive(Default)]
struct Table {
    next: u64,
    observations: HashMap<u64, Observation>,
    /// Which observations watch something on an object, so a change does not
    /// have to look through every registration in the process.
    by_object: HashMap<usize, Vec<u64>>,
}

impl Table {
    fn index(&mut self, id: u64, watches: &[(usize, Vec<u8>)]) {
        for (object, _) in watches {
            let ids = self.by_object.entry(*object).or_default();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }

    fn unindex(&mut self, id: u64, watches: &[(usize, Vec<u8>)]) {
        for (object, _) in watches {
            if let Some(ids) = self.by_object.get_mut(object) {
                ids.retain(|i| *i != id);
                if ids.is_empty() {
                    self.by_object.remove(object);
                }
            }
        }
    }

    fn insert(&mut self, observation: Observation) -> u64 {
        self.next += 1;
        let id = self.next;
        self.index(id, &observation.watches);
        self.observations.insert(id, observation);
        id
    }

    fn remove(&mut self, id: u64) {
        if let Some(observation) = self.observations.remove(&id) {
            self.unindex(id, &observation.watches);
        }
    }

    fn set_watches(&mut self, id: u64, watches: Vec<(usize, Vec<u8>)>) {
        let Some(observation) = self.observations.get_mut(&id) else {
            return;
        };
        let old = std::mem::replace(&mut observation.watches, watches);
        self.unindex(id, &old);
        let new = self.observations[&id].watches.clone();
        self.index(id, &new);
    }

    /// The observations a change of `key` on `object` is a change for, oldest first.
    fn watching(&self, object: usize, key: &[u8]) -> Vec<u64> {
        let mut ids: Vec<u64> = self
            .by_object
            .get(&object)
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| {
                self.observations[id]
                    .watches
                    .iter()
                    .any(|(o, k)| *o == object && k == key)
            })
            .collect();
        ids.sort_unstable();
        ids
    }

    /// Drop everything about an object that is going away: what observes it,
    /// and what watched it on the way along a longer path. A later object at
    /// the same address must not inherit either.
    fn forget(&mut self, object: usize) {
        let gone: Vec<u64> = self
            .observations
            .iter()
            .filter(|(_, o)| o.observed == object)
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            self.remove(id);
        }
        for id in self.by_object.remove(&object).unwrap_or_default() {
            if let Some(observation) = self.observations.get_mut(&id) {
                observation.watches.retain(|(o, _)| *o != object);
            }
        }
    }
}

fn table() -> MutexGuard<'static, Table> {
    static TABLE: LazyLock<Mutex<Table>> = LazyLock::new(Mutex::default);
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// What `-willChangeValueForKey:` saw, for the matching `did` to report.
struct Pending {
    object: usize,
    key: Vec<u8>,
    /// Each observation told, with the old value it had (retained, or nil).
    captured: Vec<(u64, Id)>,
}

thread_local! {
    /// A stack, because changes nest: a setter's `will` may come inside another
    /// key's `will`/`did` pair.
    static PENDING: RefCell<Vec<Pending>> = const { RefCell::new(Vec::new()) };
}

fn send_id(receiver: Id, sel: &str, argument: Id) -> Id {
    // SAFETY: each selector this is used with is `id (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), argument)
    }
}

fn send_void(receiver: Id, sel: &str, argument: Id) {
    // SAFETY: each selector this is used with is `void (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get(sel), argument)
    }
}

/// An `NSString` for `bytes`, released when dropped.
struct Text(Id);

impl Text {
    fn new(bytes: &[u8]) -> Text {
        Text(crate::string::from_str(&String::from_utf8_lossy(bytes)))
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        // SAFETY: a string `new` made.
        unsafe { corefoundation::api::CFRelease(self.0 as corefoundation::base::CFTypeRef) };
    }
}

fn value_for_key(object: Id, key: &[u8]) -> Id {
    send_id(object, "valueForKey:", Text::new(key).0)
}

fn value_for_path(object: Id, path: &[u8]) -> Id {
    send_id(object, "valueForKeyPath:", Text::new(path).0)
}

/// `bounds` as `Bounds`.
fn capitalized(key: &[u8]) -> String {
    let key = String::from_utf8_lossy(key);
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// Every `NSKVONotifying_` class, by the class it stands in for.
fn notifying_classes() -> &'static Mutex<HashMap<usize, &'static Class>> {
    static CLASSES: LazyLock<Mutex<HashMap<usize, &'static Class>>> = LazyLock::new(Mutex::default);
    &CLASSES
}

/// Something remembered per `(class, key)`: by the class's address and the key.
type PerKey<T> = Mutex<HashMap<(usize, Vec<u8>), T>>;

/// `(class, selector)` to the key its setter announces.
type Setters = RwLock<HashMap<(usize, usize), Vec<u8>>>;

/// The setters a notifying class wraps: `(class, selector)` to the key.
fn wrapped_setters() -> &'static Setters {
    static SETTERS: LazyLock<Setters> = LazyLock::new(RwLock::default);
    &SETTERS
}

fn is_notifying(class: &'static Class) -> bool {
    class.name_str().starts_with("NSKVONotifying_")
        && notifying_classes()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|c| std::ptr::eq(*c, class))
}

/// The class a program thinks `class` is: itself, or what it stands in for.
fn original(class: &'static Class) -> &'static Class {
    match is_notifying(class) {
        true => class.superclass().unwrap_or(class),
        false => class,
    }
}

/// The `NSKVONotifying_` subclass for `base`, made on first use.
fn notifying_class(base: &'static Class) -> &'static Class {
    let mut classes = notifying_classes()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(class) = classes.get(&(objc::class::ptr(base) as usize)) {
        return class;
    }
    let name = CString::new(format!("NSKVONotifying_{}", base.name_str()))
        .expect("a class name has no NUL");
    let class = match objc::class::allocate_pair(Some(base), &name, 0) {
        Some(class) => {
            methods!(class, {
                "class" => notifying_class_of, c"#16@0:8";
            });
            objc::class::register_pair(class);
            class
        }
        // A guest made a class of that name itself; there is nothing better to
        // use, and wrapping its setters is what it would have wanted anyway.
        None => objc::class::lookup(&name).expect("the name is taken"),
    };
    classes.insert(objc::class::ptr(base) as usize, class);
    class
}

/// `-class` on a notifying object: the class it had before.
unsafe extern "C" fn notifying_class_of(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a live object.
    unsafe { objc::object::class_of(receiver) }.map_or(std::ptr::null_mut(), |c| {
        objc::class::ptr(original(c)) as Id
    })
}

/// The key a wrapped setter announces, and the setter it wraps.
///
/// # Safety
///
/// `receiver` is a live object whose class wraps `sel`.
unsafe fn wrapped(receiver: Id, sel: Sel) -> (Vec<u8>, Imp) {
    // SAFETY: the caller's obligation.
    let class = unsafe { objc::object::class_of(receiver) }.expect("a live object");
    let setters = wrapped_setters().read().unwrap_or_else(|e| e.into_inner());
    for class in class.chain() {
        if let Some(key) = setters.get(&(objc::class::ptr(class) as usize, sel as usize))
            && let Some(method) = class.superclass().and_then(|s| s.method(sel))
        {
            return (key.clone(), method.implementation());
        }
    }
    objc::fail!(
        "KVO: -[{} {}] reached a wrapper that does not wrap it",
        class.name_str(),
        // SAFETY: a registered selector's name.
        unsafe { CStr::from_ptr(objc::sel::name(sel)) }.to_string_lossy()
    );
}

/// One wrapper per way an argument travels: in a general register, in a vector
/// register as a float or a double, as two doubles, in memory, or as two words.
macro_rules! wrapper {
    ($name:ident, $ty:ty) => {
        unsafe extern "C" fn $name(receiver: Id, sel: Sel, value: $ty) {
            // SAFETY: installed only on a notifying class, for `sel`.
            let (key, imp) = unsafe { wrapped(receiver, sel) };
            let key = Text::new(&key);
            send_void(receiver, "willChangeValueForKey:", key.0);
            // SAFETY: the setter this wraps takes one `$ty`, which is how its
            // encoding was classified.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel, $ty) = std::mem::transmute(imp);
                f(receiver, sel, value);
            }
            send_void(receiver, "didChangeValueForKey:", key.0);
        }
    };
}

wrapper!(set_word, usize);
wrapper!(set_float, f32);
wrapper!(set_double, f64);
wrapper!(set_pair, NSPoint);
wrapper!(set_rect, NSRect);
wrapper!(set_range, NSRange);

fn wrapper_for(shape: Shape) -> Imp {
    match shape {
        Shape::Object | Shape::Bool | Shape::Integer(_) => set_word as Imp,
        Shape::Float => set_float as Imp,
        Shape::Double => set_double as Imp,
        Shape::Point | Shape::Size => set_pair as Imp,
        Shape::Rect => set_rect as Imp,
        Shape::Range => set_range as Imp,
    }
}

/// Whether changes of `key` on instances of `base` notify by themselves, and
/// if so through which setter - asked of the class once and remembered.
fn automatic_setter(base: &'static Class, key: &[u8]) -> Option<(Sel, Shape, CString)> {
    type Decision = Option<(usize, Shape, CString)>;
    static DECIDED: LazyLock<PerKey<Decision>> = LazyLock::new(Mutex::default);
    let decided = &*DECIDED;
    let at = (objc::class::ptr(base) as usize, key.to_vec());
    if let Some(decision) = decided.lock().unwrap_or_else(|e| e.into_inner()).get(&at) {
        return decision
            .clone()
            .map(|(sel, shape, types)| (sel as Sel, shape, types));
    }
    let decision = (|| {
        let automatic = {
            let key = Text::new(key);
            // SAFETY: `BOOL (Class, SEL, NSString *)`.
            unsafe {
                let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
                    std::mem::transmute(objc::msg::entry());
                f(
                    objc::class::ptr(base) as Id,
                    objc::sel::get("automaticallyNotifiesObserversForKey:"),
                    key.0,
                ) != 0
            }
        };
        if !automatic {
            return None;
        }
        let sel = objc::sel::get(&crate::kvc::setter_name(&String::from_utf8_lossy(key)));
        let method = base.method(sel)?;
        // SAFETY: a method's encoding is a NUL-terminated string the runtime owns.
        let types = unsafe { CStr::from_ptr(method.types()) };
        let shape = crate::kvc::parts(types).get(3).and_then(|p| Shape::of(p))?;
        Some((sel as usize, shape, types.to_owned()))
    })();
    decided
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(at, decision.clone());
    decision.map(|(sel, shape, types)| (sel as Sel, shape, types))
}

/// Make a change of `key` on `object` announce itself, and arrange for the
/// table to hear when `object` goes away.
fn prepare(object: Id, key: &[u8]) {
    attach_sentinel(object);
    // SAFETY: a live object.
    let Some(class) = (unsafe { objc::object::class_of(object) }) else {
        return;
    };
    // A class object's setters are class methods; Apple does not swizzle those.
    if class.is_meta() {
        return;
    }
    let base = original(class);
    let Some((sel, shape, types)) = automatic_setter(base, key) else {
        return;
    };
    let notifying = notifying_class(base);
    if notifying.own_method(sel).is_none() {
        wrapped_setters()
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                (objc::class::ptr(notifying) as usize, sel as usize),
                key.to_vec(),
            );
        notifying.add_method(sel, wrapper_for(shape), &types);
    }
    if !std::ptr::eq(class, notifying) {
        // SAFETY: a subclass with no ivars of its own has the same layout.
        unsafe { objc::object::set_class(object, notifying) };
    }
}

/// The key paths `key` on `object` depends on, from its class's
/// `+keyPathsForValuesAffectingValueForKey:`.
fn affecting(object: Id, key: &[u8]) -> Vec<Vec<u8>> {
    // SAFETY: a live object.
    let Some(class) = (unsafe { objc::object::class_of(object) }) else {
        return Vec::new();
    };
    let base = original(class);
    static CACHE: LazyLock<PerKey<Vec<Vec<u8>>>> = LazyLock::new(Mutex::default);
    let cache = &*CACHE;
    let at = (objc::class::ptr(base) as usize, key.to_vec());
    if let Some(paths) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&at) {
        return paths.clone();
    }
    let set = send_id(
        objc::class::ptr(base) as Id,
        "keyPathsForValuesAffectingValueForKey:",
        Text::new(key).0,
    );
    let paths = strings_in(send_id(set, "allObjects", std::ptr::null_mut()));
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(at, paths.clone());
    paths
}

/// The strings in an `NSArray`.
fn strings_in(array: Id) -> Vec<Vec<u8>> {
    if array.is_null() {
        return Vec::new();
    }
    // SAFETY: `NSUInteger (id, SEL)` and `id (id, SEL, NSUInteger)`.
    unsafe {
        let count: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(objc::msg::entry());
        let at: unsafe extern "C" fn(Id, Sel, usize) -> Id =
            std::mem::transmute(objc::msg::entry());
        (0..count(array, objc::sel::get("count")))
            .map(|i| crate::string::bytes(at(array, objc::sel::get("objectAtIndex:"), i)))
            .collect()
    }
}

/// Every `(object, key)` a change of `path` on `object` can come from.
fn expand(object: Id, path: &[u8], depth: usize, out: &mut Vec<(usize, Vec<u8>)>) {
    if object.is_null() || depth > MAX_DEPTH {
        return;
    }
    let (first, rest) = match path.iter().position(|b| *b == b'.') {
        Some(dot) => (&path[..dot], Some(&path[dot + 1..])),
        None => (path, None),
    };
    // A collection operator is computed, not stored: nothing to watch.
    if first.first() == Some(&b'@') {
        return;
    }
    let entry = (object as usize, first.to_vec());
    let seen = out.contains(&entry);
    if !seen {
        out.push(entry);
        prepare(object, first);
        for dependent in affecting(object, first) {
            expand(object, &dependent, depth + 1, out);
        }
    }
    if let Some(rest) = rest {
        expand(value_for_key(object, first), rest, depth + 1, out);
    }
}

fn watches_for(object: Id, path: &[u8]) -> Vec<(usize, Vec<u8>)> {
    let mut out = Vec::new();
    expand(object, path, 0, &mut out);
    out
}

/// Work out again what observation `id` watches, after something along its
/// path changed.
fn rewire(id: u64) {
    let Some((observed, path)) = table()
        .observations
        .get(&id)
        .map(|o| (o.observed, o.key_path.clone()))
    else {
        return;
    };
    // Outside the lock: this asks the objects along the path for their values.
    let watches = watches_for(observed as Id, &path);
    table().set_watches(id, watches);
}

/// The key `object`'s sentinel association is stored under.
static SENTINEL_KEY: u8 = 0;

/// Give `object` an association whose release tells the table it is gone.
fn attach_sentinel(object: Id) {
    let key = &SENTINEL_KEY as *const u8 as *const c_void;
    if !objc::arc::associated(object, key).is_null() {
        return;
    }
    let class = corefoundation::base::register(
        c"_RosemaryKeyValueSentinel",
        None,
        2 * size_of::<usize>(),
        false,
    );
    if class.own_method(objc::sel::get("dealloc")).is_none() {
        methods!(class, {
            "dealloc" => sentinel_dealloc, c"v16@0:8";
        });
    }
    let sentinel = objc::object::create_instance(class, 0);
    // SAFETY: the instance is two words wide; the second is ours.
    unsafe { *(sentinel as *mut usize).add(1) = object as usize };
    objc::arc::set_associated_with_policy(object, key, sentinel, 1);
    objc::arc::release(sentinel);
}

unsafe extern "C" fn sentinel_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: the address `attach_sentinel` stored.
    let object = unsafe { *(receiver as *const usize).add(1) };
    table().forget(object);
    objc::arc::finalize(receiver);
}

/// The change dictionary handed to an observer, autoreleased.
///
/// `Some(nil)` for a value that was asked for and is nil puts `NSNull` there,
/// as Apple does; `None` leaves the entry out.
fn change_dictionary(new: Option<Id>, old: Option<Id>, prior: bool) -> Id {
    use corefoundation::base::CFTypeRef;
    let present = |value: Id| match value.is_null() {
        true => crate::value::null_object(),
        false => value,
    };
    let mut entries: Vec<(&str, Id)> = vec![("kind", crate::number(CHANGE_SETTING))];
    if let Some(new) = new {
        entries.push(("new", present(new)));
    }
    if let Some(old) = old {
        entries.push(("old", present(old)));
    }
    if prior {
        entries.push(("notificationIsPrior", crate::number(1)));
    }
    let pairs: Vec<(CFTypeRef, CFTypeRef)> = entries
        .iter()
        .map(|(key, value)| {
            (
                crate::string::from_str(key) as CFTypeRef,
                *value as CFTypeRef,
            )
        })
        .collect();
    let dictionary = corefoundation::collections::new_dictionary(
        &pairs,
        corefoundation::callbacks::Policy::from_key_callbacks(),
        corefoundation::callbacks::Policy::from_type_callbacks(),
        false,
    );
    for (key, _) in &pairs {
        // The dictionary took its own reference to each key.
        // SAFETY: a string this function made.
        unsafe { corefoundation::api::CFRelease(*key) };
    }
    // SAFETY: a fresh dictionary.
    unsafe { corefoundation::api::CFAutorelease(dictionary) as Id }
}

/// Send `-observeValueForKeyPath:ofObject:change:context:` to one observer.
fn notify(observer: Id, key_path: &[u8], observed: Id, change: Id, context: usize) {
    let sel = objc::sel::get("observeValueForKeyPath:ofObject:change:context:");
    // SAFETY: a registered observer is live until it removes itself.
    let Some(method) = (unsafe { objc::object::class_of(observer) }).and_then(|c| c.method(sel))
    else {
        // An observer with no callback is a program that registered and does not
        // want to hear, which is its own business rather than a failure here.
        return;
    };
    let path = Text::new(key_path);
    // SAFETY: the method's signature is
    // `void (id, SEL, NSString *, id, NSDictionary *, void *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id, Id, Id, usize) =
            std::mem::transmute(method.implementation());
        f(observer, sel, path.0, observed, change, context);
    }
}

/// What an observation needs to be told about, copied out of the table.
struct Snapshot {
    observed: Id,
    observer: Id,
    key_path: Vec<u8>,
    context: usize,
    options: usize,
}

fn snapshot(id: u64) -> Option<Snapshot> {
    table().observations.get(&id).map(|o| Snapshot {
        observed: o.observed as Id,
        observer: o.observer as Id,
        key_path: o.key_path.clone(),
        context: o.context,
        options: o.options,
    })
}

/// Whether anything watches `object` at all, which lets a caller skip the
/// will/did messages when nothing would hear them.
pub(crate) fn is_watched(object: Id) -> bool {
    table().by_object.contains_key(&(object as usize))
}

/// The work of `-willChangeValueForKey:`: capture each old value asked for,
/// and send the prior notifications.
pub(crate) fn will_change(object: Id, key: &[u8]) {
    // Copied out from under the lock: an observer's callback is guest code and may
    // add or remove a registration.
    let ids = table().watching(object as usize, key);
    let mut captured = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(o) = snapshot(id) else {
            continue;
        };
        let old = match o.options & options::OLD != 0 {
            true => objc::arc::retain(value_for_path(o.observed, &o.key_path)),
            false => std::ptr::null_mut(),
        };
        captured.push((id, old));
        if o.options & options::PRIOR != 0 {
            let old = (o.options & options::OLD != 0).then_some(old);
            notify(
                o.observer,
                &o.key_path,
                o.observed,
                change_dictionary(None, old, true),
                o.context,
            );
        }
    }
    PENDING.with(|pending| {
        pending.borrow_mut().push(Pending {
            object: object as usize,
            key: key.to_vec(),
            captured,
        })
    });
}

/// The work of `-didChangeValueForKey:`: rewire what watches past the change,
/// and notify.
pub(crate) fn did_change(object: Id, key: &[u8]) {
    let pending = PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        let at = pending
            .iter()
            .rposition(|p| p.object == object as usize && p.key == key)?;
        Some(pending.remove(at))
    });
    // A `did` without its `will` still notifies, only without old values.
    let captured = match pending {
        Some(pending) => pending.captured,
        None => table()
            .watching(object as usize, key)
            .into_iter()
            .map(|id| (id, std::ptr::null_mut()))
            .collect(),
    };
    for (id, _) in &captured {
        rewire(*id);
    }
    for (id, old) in captured {
        if let Some(o) = snapshot(id) {
            let new =
                (o.options & options::NEW != 0).then(|| value_for_path(o.observed, &o.key_path));
            let old_entry = (o.options & options::OLD != 0).then_some(old);
            notify(
                o.observer,
                &o.key_path,
                o.observed,
                change_dictionary(new, old_entry, false),
                o.context,
            );
        }
        objc::arc::release(old);
    }
}

/// # Safety
///
/// `key_path` is an `NSString` or nil; the rest are live objects or nil.
unsafe extern "C" fn add_observer(
    receiver: Id,
    _sel: Sel,
    observer: Id,
    key_path: Id,
    options: usize,
    context: usize,
) {
    // SAFETY: an `NSString`, by the caller.
    let path = unsafe { crate::string::bytes(key_path) };
    let watches = watches_for(receiver, &path);
    table().insert(Observation {
        observed: receiver as usize,
        observer: observer as usize,
        key_path: path.clone(),
        context,
        options,
        watches,
    });
    if options & options::INITIAL != 0 {
        let new = (options & options::NEW != 0).then(|| value_for_path(receiver, &path));
        notify(
            observer,
            &path,
            receiver,
            change_dictionary(new, None, false),
            context,
        );
    }
}

/// Remove the most recent registration matching, as Apple does; a context of
/// `None` matches any.
fn remove(observed: Id, observer: Id, key_path: Id, context: Option<usize>) {
    // SAFETY: an `NSString`, by the caller.
    let path = unsafe { crate::string::bytes(key_path) };
    let mut table = table();
    let newest = table
        .observations
        .iter()
        .filter(|(_, o)| {
            o.observed == observed as usize
                && o.observer == observer as usize
                && o.key_path == path
                && context.is_none_or(|c| c == o.context)
        })
        .map(|(id, _)| *id)
        .max();
    if let Some(id) = newest {
        table.remove(id);
    }
}

unsafe extern "C" fn remove_observer(receiver: Id, _sel: Sel, observer: Id, key_path: Id) {
    remove(receiver, observer, key_path, None);
}

unsafe extern "C" fn remove_observer_with_context(
    receiver: Id,
    _sel: Sel,
    observer: Id,
    key_path: Id,
    context: usize,
) {
    remove(receiver, observer, key_path, Some(context));
}

unsafe extern "C" fn will_change_value(receiver: Id, _sel: Sel, key: Id) {
    // SAFETY: an `NSString`, by the caller.
    will_change(receiver, &unsafe { crate::string::bytes(key) });
}

unsafe extern "C" fn did_change_value(receiver: Id, _sel: Sel, key: Id) {
    // SAFETY: an `NSString`, by the caller.
    did_change(receiver, &unsafe { crate::string::bytes(key) });
}

/// The to-many and set-mutation forms. Observers are told of a change of the
/// whole value, which is what an observer that ignores the kind already reads.
///
/// FIXME (ACCURACY): the change kind is always "setting", with no indexes.
unsafe extern "C" fn will_change_indexes(receiver: Id, _sel: Sel, _kind: usize, _i: Id, key: Id) {
    // SAFETY: an `NSString`, by the caller.
    will_change(receiver, &unsafe { crate::string::bytes(key) });
}

unsafe extern "C" fn did_change_indexes(receiver: Id, _sel: Sel, _kind: usize, _i: Id, key: Id) {
    // SAFETY: an `NSString`, by the caller.
    did_change(receiver, &unsafe { crate::string::bytes(key) });
}

unsafe extern "C" fn will_change_set(receiver: Id, _sel: Sel, key: Id, _kind: usize, _o: Id) {
    // SAFETY: an `NSString`, by the caller.
    will_change(receiver, &unsafe { crate::string::bytes(key) });
}

unsafe extern "C" fn did_change_set(receiver: Id, _sel: Sel, key: Id, _kind: usize, _o: Id) {
    // SAFETY: an `NSString`, by the caller.
    did_change(receiver, &unsafe { crate::string::bytes(key) });
}

/// `+automaticallyNotifiesObserversForKey:`: `+automaticallyNotifiesObserversOf<Key>`
/// when the class has one, and yes otherwise.
unsafe extern "C" fn automatically_notifies(class: Id, _sel: Sel, key: Id) -> u8 {
    // SAFETY: an `NSString`, by the caller.
    let key = unsafe { crate::string::bytes(key) };
    let sel = objc::sel::get(&format!(
        "automaticallyNotifiesObserversOf{}",
        capitalized(&key)
    ));
    // SAFETY: the receiver is a class, whose `isa` is its metaclass.
    let Some(method) = (unsafe { objc::object::class_of(class) }).and_then(|m| m.method(sel))
    else {
        return 1;
    };
    // SAFETY: `BOOL (Class, SEL)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(method.implementation());
        f(class, sel)
    }
}

/// `+keyPathsForValuesAffectingValueForKey:`: `+keyPathsForValuesAffecting<Key>`
/// when the class has one, and an empty set otherwise.
unsafe extern "C" fn key_paths_affecting(class: Id, _sel: Sel, key: Id) -> Id {
    // SAFETY: an `NSString`, by the caller.
    let key = unsafe { crate::string::bytes(key) };
    let sel = objc::sel::get(&format!("keyPathsForValuesAffecting{}", capitalized(&key)));
    // SAFETY: the receiver is a class, whose `isa` is its metaclass.
    if let Some(method) = (unsafe { objc::object::class_of(class) }).and_then(|m| m.method(sel)) {
        // SAFETY: `NSSet * (Class, SEL)`.
        return unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> Id =
                std::mem::transmute(method.implementation());
            f(class, sel)
        };
    }
    let set = objc::class::lookup(c"NSSet").expect("Foundation registered NSSet");
    send_id(objc::class::ptr(set) as Id, "set", std::ptr::null_mut())
}

pub(crate) fn install() {
    let nsobject = objc::class::lookup(c"NSObject").expect("the runtime's own NSObject");
    methods!(nsobject, {
        "addObserver:forKeyPath:options:context:" => add_observer, c"v48@0:8@16@24Q32^v40";
        "removeObserver:forKeyPath:" => remove_observer, c"v32@0:8@16@24";
        "removeObserver:forKeyPath:context:" => remove_observer_with_context,
        c"v40@0:8@16@24^v32";
        "willChangeValueForKey:" => will_change_value, c"v24@0:8@16";
        "didChangeValueForKey:" => did_change_value, c"v24@0:8@16";
        "willChange:valuesAtIndexes:forKey:" => will_change_indexes, c"v40@0:8Q16@24@32";
        "didChange:valuesAtIndexes:forKey:" => did_change_indexes, c"v40@0:8Q16@24@32";
        "willChangeValueForKey:withSetMutation:usingObjects:" => will_change_set,
        c"v40@0:8@16Q24@32";
        "didChangeValueForKey:withSetMutation:usingObjects:" => did_change_set,
        c"v40@0:8@16Q24@32";
    });
    class_methods!(nsobject, {
        "automaticallyNotifiesObserversForKey:" => automatically_notifies, c"c24@0:8@16";
        "keyPathsForValuesAffectingValueForKey:" => key_paths_affecting, c"@24@0:8@16";
    });
}

/// The change-dictionary keys, as `(Mach-O name, value)`.
///
/// Each is an `NSString *` **variable**, so an unresolved one binds to zero and the
/// observer passes address zero as the key it looks its new value up by. The values
/// are Apple's: `NSKeyValueChangeNewKey` is the string `"new"`.
pub const KEYS: &[(&str, &str)] = &[
    ("_NSKeyValueChangeKindKey", "kind"),
    ("_NSKeyValueChangeNewKey", "new"),
    ("_NSKeyValueChangeOldKey", "old"),
    ("_NSKeyValueChangeIndexesKey", "indexes"),
    (
        "_NSKeyValueChangeNotificationIsPriorKey",
        "notificationIsPrior",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// What each observer was told, as `(observer, "path new old")`.
    ///
    /// Shared by every test in this module, which run in parallel - so each
    /// test reads back only what its own observer was told.
    static SEEN: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());

    fn seen(observer: Id) -> Vec<String> {
        SEEN.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(o, _)| *o == observer as usize)
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    /// A value as the tests compare it: its number or its text, `null` for
    /// `NSNull`, and `-` for absent.
    fn text(value: Id) -> String {
        if value.is_null() {
            return "-".into();
        }
        // SAFETY: a live object from a change dictionary.
        let class = unsafe { objc::object::class_of(value) }.expect("an object");
        if class.name_str() == "NSNull" {
            return "null".into();
        }
        // SAFETY: as above.
        if let Some(text) = unsafe { corefoundation::string::as_string(value as _) } {
            return String::from_utf8_lossy(&text.utf8()).into_owned();
        }
        // SAFETY: anything else here is a number.
        unsafe { crate::number_of(value) }.map_or("?".into(), |n| n.to_string())
    }

    unsafe extern "C" fn observed(
        receiver: Id,
        _sel: Sel,
        key_path: Id,
        _object: Id,
        change: Id,
        _context: usize,
    ) {
        // SAFETY: the key path is the `NSString` `notify` made.
        let path = String::from_utf8_lossy(&unsafe { crate::string::bytes(key_path) }).into_owned();
        let entry = format!(
            "{path} {} {}",
            text(value_for_key(change, b"new")),
            text(value_for_key(change, b"old"))
        );
        SEEN.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((receiver as usize, entry));
    }

    fn observer() -> Id {
        crate::bootstrap();
        let class = corefoundation::base::register(c"RosemaryKvoObserver", None, 8, false);
        class.replace_method(
            objc::sel::get("observeValueForKeyPath:ofObject:change:context:"),
            observed as *const c_void,
            c"v48@0:8@16@24@32^v40",
        );
        objc::object::create_instance(class, 0)
    }

    /// A person: `name` (an object), `age` (an integer) and `friend` (another
    /// person) in slots after `isa`, and `summary`, computed from `name`.
    #[repr(C)]
    struct Person {
        isa: usize,
        name: Id,
        age: i64,
        friend: Id,
    }

    unsafe extern "C" fn name(receiver: Id, _sel: Sel) -> Id {
        unsafe { (*(receiver as *mut Person)).name }
    }
    unsafe extern "C" fn set_name(receiver: Id, _sel: Sel, value: Id) {
        unsafe { (*(receiver as *mut Person)).name = value };
    }
    unsafe extern "C" fn age(receiver: Id, _sel: Sel) -> i64 {
        unsafe { (*(receiver as *mut Person)).age }
    }
    unsafe extern "C" fn set_age(receiver: Id, _sel: Sel, value: i64) {
        unsafe { (*(receiver as *mut Person)).age = value };
    }
    unsafe extern "C" fn friend(receiver: Id, _sel: Sel) -> Id {
        unsafe { (*(receiver as *mut Person)).friend }
    }
    unsafe extern "C" fn set_friend(receiver: Id, _sel: Sel, value: Id) {
        unsafe { (*(receiver as *mut Person)).friend = value };
    }
    unsafe extern "C" fn summary(receiver: Id, _sel: Sel) -> Id {
        let name = unsafe { (*(receiver as *mut Person)).name };
        let name = String::from_utf8_lossy(&unsafe { crate::string::bytes(name) }).into_owned();
        crate::string::from_str(&format!("person {name}"))
    }
    unsafe extern "C" fn affecting_summary(_class: Id, _sel: Sel) -> Id {
        let set = objc::class::lookup(c"NSSet").expect("NSSet");
        send_id(
            objc::class::ptr(set) as Id,
            "setWithObject:",
            crate::string::from_str("name"),
        )
    }

    fn person_class() -> &'static Class {
        crate::bootstrap();
        let class =
            corefoundation::base::register(c"RosemaryKvoPerson", None, size_of::<Person>(), false);
        methods!(class, {
            "name" => name, c"@16@0:8";
            "setName:" => set_name, c"v24@0:8@16";
            "age" => age, c"q16@0:8";
            "setAge:" => set_age, c"v24@0:8q16";
            "friend" => friend, c"@16@0:8";
            "setFriend:" => set_friend, c"v24@0:8@16";
            "summary" => summary, c"@16@0:8";
        });
        class_methods!(class, {
            "keyPathsForValuesAffectingSummary" => affecting_summary, c"@16@0:8";
        });
        class
    }

    fn person() -> Id {
        objc::object::create_instance(person_class(), 0)
    }

    fn observe(subject: Id, observer: Id, path: &str, options: usize) {
        let path = Text::new(path.as_bytes());
        // SAFETY: live objects this test made.
        unsafe {
            add_observer(
                subject,
                objc::sel::get("addObserver:forKeyPath:options:context:"),
                observer,
                path.0,
                options,
                0,
            )
        };
    }

    fn set(subject: Id, setter: &str, value: usize) {
        // SAFETY: `void (id, SEL, word)`, which every setter here is.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, usize) = std::mem::transmute(objc::msg::entry());
            f(subject, objc::sel::get(setter), value);
        }
    }

    fn class_answer(object: Id) -> Id {
        send_id(object, "class", std::ptr::null_mut())
    }

    /// A plain setter notifies once the object is observed, with the old and
    /// new values, and the object still answers `-class` as before.
    #[test]
    fn a_setter_notifies_with_old_and_new_values() {
        let subject = person();
        let watcher = observer();
        set(subject, "setAge:", 30);
        observe(subject, watcher, "age", options::NEW | options::OLD);
        set(subject, "setAge:", 31);
        assert_eq!(seen(watcher), ["age 31 30"]);
        assert_eq!(
            class_answer(subject),
            objc::class::ptr(person_class()) as Id,
            "the notifying subclass is hidden from -class"
        );
        // SAFETY: a live object.
        let isa = unsafe { objc::object::class_of(subject) }.expect("an object");
        assert_eq!(isa.name_str(), "NSKVONotifying_RosemaryKvoPerson");
    }

    /// Once removed, the setter still works and nothing is told.
    #[test]
    fn a_removed_observer_is_not_told() {
        let subject = person();
        let watcher = observer();
        observe(subject, watcher, "age", options::NEW);
        let path = Text::new(b"age");
        // SAFETY: live objects this test made.
        unsafe {
            remove_observer(
                subject,
                objc::sel::get("removeObserver:forKeyPath:"),
                watcher,
                path.0,
            )
        };
        set(subject, "setAge:", 5);
        assert!(seen(watcher).is_empty());
        // SAFETY: a live object of the class.
        assert_eq!(unsafe { age(subject, objc::sel::get("age")) }, 5);
    }

    /// `friend.age` is told of a change to the friend's age, and of a new
    /// friend - and after that, of the new friend's changes, not the old one's.
    #[test]
    fn a_dotted_path_follows_the_object_along_it() {
        let subject = person();
        let first = person();
        let second = person();
        let watcher = observer();
        set(first, "setAge:", 1);
        set(second, "setAge:", 2);
        set(subject, "setFriend:", first as usize);
        observe(subject, watcher, "friend.age", options::NEW);
        set(first, "setAge:", 10);
        set(subject, "setFriend:", second as usize);
        set(first, "setAge:", 11);
        set(second, "setAge:", 20);
        assert_eq!(
            seen(watcher),
            ["friend.age 10 -", "friend.age 2 -", "friend.age 20 -"]
        );
    }

    /// `summary` depends on `name` through `+keyPathsForValuesAffectingSummary`.
    #[test]
    fn a_dependent_key_is_told_when_what_it_depends_on_changes() {
        let subject = person();
        let watcher = observer();
        observe(subject, watcher, "summary", options::NEW);
        set(subject, "setName:", crate::string::from_str("ada") as usize);
        assert_eq!(seen(watcher), ["summary person ada -"]);
    }

    /// A nil value asked for arrives as `NSNull`.
    #[test]
    fn a_nil_value_arrives_as_null() {
        let subject = person();
        let watcher = observer();
        observe(subject, watcher, "name", options::NEW | options::OLD);
        set(subject, "setName:", crate::string::from_str("x") as usize);
        assert_eq!(seen(watcher), ["name x null"]);
    }

    /// `NSKeyValueObservingOptionInitial` notifies at registration.
    #[test]
    fn the_initial_option_notifies_straight_away() {
        let subject = person();
        let watcher = observer();
        set(subject, "setAge:", 7);
        observe(subject, watcher, "age", options::INITIAL | options::NEW);
        assert_eq!(seen(watcher), ["age 7 -"]);
    }

    /// Manual `will`/`did` on a key with no setter still notifies.
    #[test]
    fn an_announced_change_notifies() {
        let subject = person();
        let watcher = observer();
        observe(subject, watcher, "announced", 0);
        let key = Text::new(b"announced");
        send_void(subject, "willChangeValueForKey:", key.0);
        send_void(subject, "didChangeValueForKey:", key.0);
        assert_eq!(seen(watcher), ["announced - -"]);
    }

    /// An observed object that goes away takes its registrations with it.
    #[test]
    fn a_destroyed_object_leaves_nothing_behind() {
        let subject = person();
        let watcher = observer();
        observe(subject, watcher, "age", 0);
        assert!(is_watched(subject));
        objc::arc::release(subject);
        assert!(!is_watched(subject));
    }
}
