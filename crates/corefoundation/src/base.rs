use std::ffi::c_void;

use objc::class::Class;
use objc::object::Id;

/// A retained, non-null CoreFoundation object.
pub type CFTypeRef = *const c_void;

/// `CFIndex`: a signed `long`, and negative for "not found".
pub type CFIndex = isize;

/// `CFTypeID`. Apple's is a small integer; here it is the class pointer, which
/// is the same thing said more directly - `CFGetTypeID(x) == CFStringGetTypeID()`
/// asks whether `x` is a string, and the class answers that exactly.
pub type CFTypeID = usize;

/// The type ID of `class`.
pub fn type_id(class: &'static Class) -> CFTypeID {
    objc::class::ptr(class) as usize
}

/// The classes that are CoreFoundation types rather than private subclasses
/// of one.
///
/// `__NSCFConstantString` is a `__NSCFString` and must answer
/// `CFStringGetTypeID()`, not a type ID of its own: a guest that switches on
/// `CFGetTypeID` - and `libYoYoGamepad` does, on every value IOKit hands it -
/// would otherwise fall through every arm for a string that came from an image
/// rather than the heap.
static TYPES: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// The name of each registered type, beside its id.
///
/// `CFCopyTypeIDDescription` is asked for a type's *name*, and the class is
/// where the name lives, so the two are recorded together.  Without the name
/// that function could only say "some type", which is the one answer a caller
/// logging an unexpected type can do without.
static TYPE_NAMES: std::sync::Mutex<Vec<(usize, &'static std::ffi::CStr)>> =
    std::sync::Mutex::new(Vec::new());

/// The name of the type `type_id` names, if this runtime registered it.
pub fn type_name(type_id: CFTypeID) -> Option<&'static str> {
    TYPE_NAMES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(id, _)| *id == type_id)
        .map(|(_, name)| name.to_str().unwrap_or("?"))
}

fn note_type(class: &'static Class) {
    {
        let mut names = TYPE_NAMES.lock().unwrap_or_else(|e| e.into_inner());
        let id = objc::class::ptr(class) as usize;
        if !names.iter().any(|(known, _)| *known == id) {
            // The class's name is a leaked NUL-terminated string, and a
            // `&'static Class` outlives the process, so this is `&'static`.
            // SAFETY: `Class::name` is set at registration to a leaked C string.
            let name = unsafe { std::ffi::CStr::from_ptr(class.name()) };
            names.push((id, name));
        }
    }
    let mut types = TYPES.lock().unwrap_or_else(|e| e.into_inner());
    let id = objc::class::ptr(class) as usize;
    // Idempotent, because a `...GetTypeID` a guest can call in a loop would
    // otherwise grow this by one entry per call. `type_id_of` only ever asks
    // whether an id is present, so a repeated note is a leak with no other
    // effect - but it is a leak in a function a program may call at will.
    if types.contains(&id) {
        return;
    }
    types.push(id);
}

/// The class of a CoreFoundation type whose class is **Foundation's**, and which
/// is therefore marked as a CoreFoundation type on the way out.
///
/// `NSNull` and `NSError` are the two such types: this crate publishes
/// `CFNullGetTypeID` and `CFErrorGetTypeID` for them while Foundation owns the
/// objects, the way Apple's CoreFoundation owns neither and toll-free bridges to
/// both. Marking the class is what makes the two answers agree - `CFGetTypeID` of
/// such an object has to equal the `...GetTypeID` that names its type, and without
/// the note it answers zero, which is also what it answers for every object that
/// is not a CoreFoundation type at all.
///
/// Where Foundation has not registered the class, it is registered here so that a
/// process linking only this crate still has a nonzero, stable type ID for a type
/// it has no instances of. `size` is the instance size Foundation's own
/// registration uses, so a process that *does* have Foundation gets that class
/// and this registration has cost nothing: [`register`] hands back a name that is
/// already taken.
pub fn bridged_class(name: &std::ffi::CStr, size: usize) -> &'static Class {
    let class = match objc::class::lookup(name) {
        Some(class) => class,
        None => register(name, None, size, true),
    };
    // The class may be Foundation's, in which case the `is_type` above never ran.
    note_type(class);
    class
}

/// The `CFTypeID` an object of `class` reports: the nearest ancestor that is a
/// CoreFoundation type, or zero for something that is not one at all.
pub fn type_id_of(class: &'static Class) -> CFTypeID {
    let types = TYPES.lock().unwrap_or_else(|e| e.into_inner());
    class
        .chain()
        .map(|c| objc::class::ptr(c) as usize)
        .find(|c| types.contains(c))
        .unwrap_or(0)
}

/// The class of `object`, or `None` for null.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
pub unsafe fn class_of(object: CFTypeRef) -> Option<&'static Class> {
    unsafe { objc::object::class_of(object as Id) }
}

/// Whether `object` is an instance of `class` or one of its subclasses.
///
/// # Safety
///
/// As [`class_of`].
pub unsafe fn is_kind_of(object: CFTypeRef, class: &'static Class) -> bool {
    unsafe { class_of(object) }.is_some_and(|c| c.chain().any(|k| std::ptr::eq(k, class)))
}

/// Register a CoreFoundation class: a subclass of `NSObject` with no ivars of
/// the runtime's own.
///
/// Every CF object's layout is a `#[repr(C)]` struct in this crate rather than a
/// list of ivars, because the layouts are not ours to choose - a `__cfstring`
/// literal in the guest's `__DATA` is four words the compiler laid out, and
/// nothing gets a chance to convert it.
/// An abstract class with no storage of its own, created if it is not there.
///
/// `NSArray` and `NSMutableArray` are registered here rather than in a
/// Foundation crate, for the same reason Apple's CoreFoundation registers them:
/// `__NSCFArray` has to descend from them or a guest's `isKindOfClass:` and its
/// `_OBJC_CLASS_$_NSArray` binding do not agree with the object it is holding.
/// Foundation then adds the methods. The two crates meet in the runtime's class
/// table rather than through a dependency, which is what lets the arrow point
/// this way at all.
pub fn abstract_class(name: &std::ffi::CStr, superclass: Option<&'static Class>) -> &'static Class {
    register(name, superclass, size_of::<usize>(), false)
}

/// The chain `NSFoo` -> `NSMutableFoo`, for a concrete class to descend from.
pub fn mutable_pair(immutable: &std::ffi::CStr, mutable: &std::ffi::CStr) -> &'static Class {
    let base = abstract_class(immutable, None);
    abstract_class(mutable, Some(base))
}

pub fn register(
    name: &std::ffi::CStr,
    superclass: Option<&'static Class>,
    instance_size: usize,
    is_type: bool,
) -> &'static Class {
    objc::bootstrap();
    if let Some(existing) = objc::class::lookup(name) {
        return noted(existing, is_type);
    }
    // Held across the check and the publish, and it is the whole of this lock's
    // reason to exist. `objc::class::allocate_pair` checks the name while
    // holding the runtime's own allocation lock, and `register_pair` publishes
    // the class *after* that lock has been released - so two threads registering
    // one name can both pass the check and both publish, leaving two classes of
    // one name in the table with whichever published last the one every lookup
    // finds. That is not a guess: `tests::the_null_type_id_is_the_class_of_the_null`
    // asks `CFNullGetTypeID` twice in one test and was handed two different
    // class pointers, 256 MB apart, in about 3 runs in 60.
    //
    // The definitive fix is in `objc`, where the two steps belong under its one
    // lock - reported rather than done, because `crates/objc` is not this
    // crate's file. Everything registered through here is closed either way.
    let _registering = REGISTERING.lock().unwrap_or_else(|e| e.into_inner());
    // Checked again now that the name cannot be taken underneath: the fast path
    // above is only an optimisation, and this is the answer.
    if let Some(existing) = objc::class::lookup(name) {
        return noted(existing, is_type);
    }
    let superclass = superclass.unwrap_or_else(|| {
        objc::class::lookup(c"NSObject").expect("NSObject is the runtime's own")
    });
    // The extra bytes are everything past what the superclass already accounts
    // for. `class_addIvar` is not used: the fields are this crate's, and naming
    // them to the runtime would only invite something to move them.
    let extra = instance_size.saturating_sub(superclass.instance_size());
    match objc::class::allocate_pair(Some(superclass), name, extra) {
        Some(class) => {
            objc::class::register_pair(class);
            noted(class, is_type)
        }
        // Another thread registered this name between the lookup above and
        // here. Its class is as good as this one would have been, and two
        // classes of one name is the outcome that must not happen.
        //
        // The `noted` is not decoration: that other thread may not have got as
        // far as `note_type` yet, and returning its class un-noted makes
        // `type_id_of` answer zero for the type - so a `CFGetTypeID` racing a
        // `...GetTypeID` gets zero for an object that *is* that type, and the two
        // disagree about an object neither of them is wrong about.
        None => noted(
            objc::class::lookup(name).expect("taken by another thread"),
            is_type,
        ),
    }
}

/// Held across a name being checked and a class of that name being published.
///
/// See [`register`], which is the only thing that takes it.
static REGISTERING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `class`, marked as a CoreFoundation type when `is_type` says it is one.
fn noted(class: &'static Class, is_type: bool) -> &'static Class {
    if is_type {
        note_type(class);
    }
    class
}

/// Mark `class` as the CoreFoundation type its subclasses report.
///
/// Used where the type is the *abstract* class - `CFGetTypeID` of any array
/// answers `CFArrayGetTypeID()`, whether the object is a `__NSCFArray` or
/// something a Foundation crate added underneath `NSArray` later.
pub fn note_type_class(class: &'static Class) {
    note_type(class);
}

/// Allocate an instance of `class` whose body is `T`, and hand back the body.
///
/// The returned pointer is the object *and* the struct: `T`'s first field is the
/// `isa`, which is what makes a CF object an Objective-C one.
///
/// # Safety
///
/// `T` must begin with an `isa`-sized field and be the layout `class` was
/// registered with.
pub unsafe fn allocate<T>(class: &'static Class) -> *mut T {
    // A class registered abstract has room for its `isa` and nothing else, so
    // writing `T` into one walks off the end of the allocation and corrupts the
    // heap - which surfaces as a trap in `malloc` somewhere else entirely. The
    // size is what the registration decided, so this is a mismatch between two
    // lines of this runtime and belongs here rather than in a caller.
    assert!(
        class.instance_size() >= size_of::<T>(),
        "{} was registered with {} bytes but is allocated as {} of {}",
        class.name_str(),
        class.instance_size(),
        std::any::type_name::<T>(),
        size_of::<T>(),
    );
    let object = objc::object::create_instance(class, 0);
    object as *mut T
}

/// Handles this tree hands out that are *not* objects.
///
/// An `IOHIDDeviceRef` is an `Arc` and an `IOHIDElementRef` is a Rust struct:
/// neither begins with an isa, and a guest is entitled to pass either to
/// `CFRetain`, because both are CoreFoundation types as far as its headers are
/// concerned.
/// Without this they look exactly like an object some other CoreFoundation made
/// - the isa check in [`crate::foreign`] cannot tell them apart - and retaining
/// one calls Apple's `objc_retain` on a structure this crate laid out.
///
/// Retaining and releasing one is a no-op: a manager, a device and its element
/// handles live for as long as the subsystem.
///
/// # The key is a bare address, and that is only sound while entries die with
/// their allocation
///
/// A heap address identifies an allocation only until the allocation is freed,
/// and the allocator will hand the same address out again. An entry left behind
/// by a freed allocation therefore claims a *different* object, and every
/// question asked through [`is_opaque`] is then answered about the wrong thing -
/// silently, because a retain count is not something a caller checks. So
/// [`forget_opaque`] is not a tidy-up: it is what makes this table mean
/// "a live handle of this tree" rather than "an address some handle once had".
/// Every kind of handle noted here must call it when the handle is destroyed;
/// [`crate::runloop`] does it from `Drop`, and a handle that is deliberately
/// immortal (a device, a pasteboard) needs no call because its allocation never
/// dies.
///
/// # Which leaves a second defect, and this is the one a leak fix does not close
///
/// Pairing every note with a forget is necessary and not sufficient, because the
/// pair cannot always be made: a handle the C world owns is only released if the
/// guest releases it, and until it does the entry is a claim on an address that
/// will be handed out again the moment the loop that holds the handle lets go of
/// it. Address reuse is therefore not a leak to be tidied up - it is reachable
/// with every entry paired perfectly, and the failure is the worst kind there is:
/// `CFRetain` on a live object returns normally having done nothing, and
/// `CFGetRetainCount` then answers `1` forever.
///
/// So the table does not trust the address on its own. Each entry may carry a
/// **witness**: a function the handle's *owner* supplies, which can tell from
/// the allocation itself whether it is still the thing that was noted there.
/// [`crate::runloop`] supplies one for all four of its kinds, and it is exact -
/// the word it reads is one the owner wrote and nothing else ever writes, and
/// what that word must hold is a value nothing else in the process can carry.
/// Where an owner has not supplied one the answer is still the table's, and that
/// is a stated limitation rather than a silent one: the only owners that have not
/// are the ones whose allocation cannot die, which is precisely the case in which
/// the bare key was sound to begin with.
static OPAQUE: std::sync::Mutex<std::collections::BTreeMap<usize, Entry>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The owner's proof that an address still holds the handle that was noted there.
///
/// # Safety
///
/// `handle` must be null, or a pointer to at least one readable word of the
/// allocation the owner laid out.
type Witness = unsafe fn(CFTypeRef) -> bool;

/// Give back the one strong reference the C world was handed.
///
/// # Safety
///
/// `handle` must be the address [`note_c_owned`] was given for a `T`, and the C
/// world must hold exactly the reference that was handed to it - once.
type Release = unsafe fn(CFTypeRef);

/// What the C world asked for, and what this table can say about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CRef {
    /// Not a handle this table is counting references for. The caller falls
    /// through to the ordinary path.
    Unknown,
    /// Counted: the reference was taken, or given back with others still held.
    Counted,
    /// A handle whose last reference this runtime has already been given back.
    /// A second one is a guest bug, and there is no answer to give.
    Spent,
}

/// What the table knows about one handle.
#[derive(Clone, Copy)]
struct Entry {
    /// The CoreFoundation type this handle reports, `0` for an untyped one.
    type_id: CFTypeID,
    /// The owner's proof that this address is still that handle, or `None` for an
    /// owner who supplied none - see the note on [`OPAQUE`].
    witness: Option<Witness>,
    /// Give the C world's reference back, if it was handed one. Cleared as it
    /// fires, so it can fire once.
    release: Option<Release>,
    /// How many references the C world holds, counted independently of
    /// [`Entry::release`] so that an owner with no way to drop its allocation can
    /// still be retained and released truthfully. `None` for a handle that is not
    /// a reference at all, and `Some(0)` for one whose references are all given
    /// back.
    refs: Option<CFIndex>,
}

impl Entry {
    /// A handle nobody owns a reference to and whose owner supplied no witness.
    const fn plain(type_id: CFTypeID) -> Entry {
        Entry {
            type_id,
            witness: None,
            release: None,
            refs: None,
        }
    }
}

/// How many handles are in [`OPAQUE`], for the tests that watch it shrink.
///
/// `note` and `forget` are not otherwise countable from outside, and a leak in a
/// table nothing reads is exactly the kind of thing a green suite cannot see.
pub fn opaque_count() -> usize {
    OPAQUE.lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Record a handle before it is handed to a guest.
pub fn note_opaque(handle: *const c_void) {
    note_typed_opaque(handle, 0);
}

/// Record an opaque handle and the CoreFoundation type it reports.
///
/// IOKit's references are CF types even though their implementation here is a
/// Rust allocation with no Objective-C `isa`. Keeping the type beside the
/// address lets `CFGetTypeID` answer without reading the allocation as an
/// object.
///
/// # The witness is not optional for a handle whose allocation can die
///
/// A handle noted here has no way to prove it is still itself, so an entry left
/// here by a freed allocation claims whatever the allocator built next. The fix
/// belongs with the owner, which is the only party that knows the layout:
/// [`note_witnessed`] takes a witness, and an owner whose allocation is immortal
/// (a device, a pasteboard, a `CFSocket`) has nothing to prove and so needs
/// nothing.
pub fn note_typed_opaque(handle: *const c_void, type_id: CFTypeID) {
    note_witnessed(handle, type_id, None);
}

/// [`note_typed_opaque`], with the owner's own proof that the address is still
/// this handle.
///
/// `witness` is asked by [`is_opaque`] and by [`witnessed`], and nothing else.
/// It must be cheap, side-effect free, and it must answer `false` for any
/// address that is not the allocation it was registered for - which means it has
/// to read a word the owner itself wrote, not one it inferred, **and** the
/// value that word must hold has to be one nothing else in the process can
/// hold. The second half is not a restatement of the first: a witness that reads
/// a word it wrote and compares it to a value other allocations also carry - zero
/// is the obvious one - is a check that answers by luck, which is the defect
/// [`is_opaque`] exists to refuse. See [`crate::runloop::HANDLE_TAG`].
///
/// # Safety
///
/// If `witness` is `Some`, it must satisfy [`Witness`]'s contract for every
/// address, including one that has since been freed and handed out again.
pub fn note_witnessed(handle: *const c_void, type_id: CFTypeID, witness: Option<Witness>) {
    if handle.is_null() {
        return;
    }
    OPAQUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(handle as usize, Entry { witness, ..Entry::plain(type_id) });
    #[cfg(test)]
    if std::env::var_os("ROSETTA3_PROBE_OPAQUE").is_some() {
        // The origin of an entry, recorded rather than inferred: `is_opaque` says
        // a handle is one of these, and *who put it here* is the only thing that
        // distinguishes a live handle from an address the allocator handed out a
        // second time.
        PROVENANCE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle as usize, std::backtrace::Backtrace::force_capture().to_string());
    }
}

/// Whether the table holds a witness for `handle` and it still vouches.
///
/// The question [`crate::runloop::is_noted`] asks, and it is asked of the *shared*
/// table rather than of that module's own registry on purpose: the two are
/// written independently - a `CFMachPort` overwrites the shared entry without
/// the run loop's ever hearing of it - and a kind registry that has been
/// outvoted by the table it sits beside must not answer.
pub fn witnessed(handle: CFTypeRef) -> bool {
    match raw_entry(handle) {
        Some(entry) => entry
            .witness
            .is_some_and(|witness| unsafe { (witness)(handle) }),
        None => false,
    }
}

/// The table's entry for `handle`, with no witness asked.
fn raw_entry(handle: CFTypeRef) -> Option<Entry> {
    if handle.is_null() {
        return None;
    }
    OPAQUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(handle as usize))
        .copied()
}

/// Where a noted handle came from, for the probe in [`is_opaque`].
///
/// Off unless `ROSETTA3_PROBE_OPAQUE` is set, and only in a test build: the
/// default output of the suite is byte-identical with this present.
#[cfg(test)]
static PROVENANCE: std::sync::Mutex<std::collections::BTreeMap<usize, String>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// The recorded origin of `handle`, if the probe recorded one.
#[cfg(test)]
fn provenance_of(handle: CFTypeRef) -> Option<String> {
    PROVENANCE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(handle as usize))
        .cloned()
}

/// Forget one, for the handles that are freed rather than immortal.
///
/// Called from the death of the handle, never from the end of its *use*: see the
/// note on [`OPAQUE`] for why the pairing with [`note_typed_opaque`] is what makes
/// the table trustworthy.
pub fn forget_opaque(handle: *const c_void) {
    OPAQUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(handle as usize));
    #[cfg(test)]
    if std::env::var_os("ROSETTA3_PROBE_OPAQUE").is_some() {
        PROVENANCE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(handle as usize));
    }
}

/// Record a handle the C world owns one reference to, and how to give it back.
///
/// This is the other half of the pairing, and it exists because the guest is the
/// only party that can end a handle the C world holds. `note`/`forget` from
/// [`crate::runloop`]'s `Drop` cover a handle whose last `Arc` this tree is
/// holding; a `CFRunLoopSourceRef` is a `leak`ed `Arc` the *guest* owns, and it
/// stays in the table until the guest's `CFRelease` arrives. Without a path to
/// that release, "entries die with their allocation" is unreachable for every
/// handle the guest made - which is most of them - and the bare-address key stays
/// exposed for exactly the handles it has to be sound for.
///
/// `release` is the owner's: it drops the reference the C world was handed, and
/// if that was the last one the allocation dies and the owner's `Drop` calls
/// [`forget_opaque`]. It is called with no lock held, because reaching
/// [`forget_opaque`] takes this table's lock.
///
/// # Safety
///
/// `release` must satisfy [`Release`], and `handle` must be an address already
/// in the table.
pub fn note_c_owned(handle: *const c_void, release: Release) {
    let mut table = OPAQUE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = table.get_mut(&(handle as usize)) {
        entry.release = Some(release);
        entry.refs = Some(1);
    }
}

/// Take one more reference to a handle the C world owns.
///
/// [`CRef::Spent`] for a handle whose references are all given back, and the
/// caller is expected to refuse rather than to fall through: the allocation is
/// gone, and `objc_retain` on freed memory is not an answer.
pub fn retain_opaque(handle: CFTypeRef) -> CRef {
    if handle.is_null() {
        return CRef::Unknown;
    }
    let mut table = OPAQUE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = table.get_mut(&(handle as usize)) else {
        return CRef::Unknown;
    };
    // A disclaimed entry is not this module's handle any more, so it has no
    // references to account for. Asking before the count is what keeps a
    // tombstone from refusing a *new* allocation built at the same address.
    if entry
        .witness
        .is_some_and(|witness| !unsafe { (witness)(handle) })
    {
        return CRef::Unknown;
    }
    match entry.refs {
        None => CRef::Unknown,
        Some(0) => CRef::Spent,
        Some(refs) => {
            entry.refs = Some(refs + 1);
            CRef::Counted
        }
    }
}

/// Give one reference back, and free the allocation when that was the last one.
///
/// ## What an over-release does, and why nothing cleverer is done about it
///
/// The count saturates at zero rather than underflowing, and a release that
/// arrives when the count is *already* zero is [`CRef::Spent`] - refused by name
/// by the caller, because the two other answers are a use-after-free and a lie.
///
/// Beyond that there is nothing to be done, and the reason is the whole of this
/// module: once the last reference goes, the owner's `Drop` removes the entry,
/// and after that the address is indistinguishable from any other address the
/// allocator owns. A guest that releases the same handle a second time is
/// therefore a use-after-free here exactly as it is in Apple's CoreFoundation,
/// and it crashes exactly as it does there. The alternative - remembering the
/// address - was written and then deleted: a remembered address is refused for
/// whichever *new* allocation lands on it, which is this module's original bug
/// with the sign flipped, and the addresses it would have to remember are the
/// ones the allocator is about to reuse.
///
/// So this is a choice between a crash on a guest bug and a crash on a correct
/// guest, and the correct guest is the one that matters.
///
/// # Safety
///
/// `handle` must be a handle the C world holds a reference to, or one it never
/// did: the count is what decides, not the caller.
pub fn release_opaque(handle: CFTypeRef) -> CRef {
    if handle.is_null() {
        return CRef::Unknown;
    }
    let release = {
        let mut table = OPAQUE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = table.get_mut(&(handle as usize)) else {
            return CRef::Unknown;
        };
        // A disclaimed entry is not this module's handle any more, so it has no
        // references to account for. Asking before the count is what keeps a
        // tombstone from refusing a *new* allocation built at the same address.
        if entry
            .witness
            .is_some_and(|witness| !unsafe { (witness)(handle) })
        {
            return CRef::Unknown;
        }
        let Some(refs) = entry.refs else {
            return CRef::Unknown;
        };
        if refs == 0 {
            return CRef::Spent;
        }
        entry.refs = Some(refs - 1);
        if refs > 1 {
            return CRef::Counted;
        }
        // The C world is done with it either way. Clearing `release` first makes
        // this monotone: a handle a run loop still holds outlives the entry, and
        // must not be given back a second time.
        entry.release.take()
    };
    if let Some(release) = release {
        // SAFETY: the owner's own function, called with the address it was
        // registered for and the one reference it was handed. No lock is held,
        // because the `Drop` this reaches calls `forget_opaque`.
        unsafe { release(handle) };
    }
    CRef::Counted
}

/// Whether `handle` is a handle whose C references have all been given back while
/// the allocation is still alive.
///
/// Witness-gated, and the gate is what makes this safe to ask: without it a
/// tombstone at a recycled address would claim whichever new allocation landed
/// there, which is this module's original bug with the sign flipped. With it, the
/// only handles this can be true of are ones the owner still recognises.
pub fn spent(handle: CFTypeRef) -> bool {
    if handle.is_null() {
        return false;
    }
    let mut table = OPAQUE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(entry) = table.get_mut(&(handle as usize)) else {
        return false;
    };
    if entry.refs != Some(0) {
        return false;
    }
    match entry.witness {
        // SAFETY: the owner's own function, for the address it was registered
        // for, under the contract in `note_witnessed`.
        Some(witness) => unsafe { (witness)(handle) },
        None => true,
    }
}

/// How many references the C world holds to `handle`, when it keeps a count for it.
///
/// `Some(0)` for a handle whose references have all been given back while the
/// allocation is still alive, and `None` for a handle this table keeps no count
/// for at all - which is every handle that is not a reference: a device, a
/// pasteboard, a `CFSocket`.
///
/// ## Why this one asks the owner too
///
/// Every other reader of this table asks the owner's witness first - `is_opaque`,
/// `opaque_type_id`, `witnessed`, `retain_opaque`, `release_opaque`, `spent` - and
/// this was the single reader that did not, which made it the one place a stale
/// entry could still produce a number. That matters more here than anywhere else,
/// because `CFGetRetainCount` asks *this* before it asks `is_opaque`, so a count
/// read out of an entry whose owner disclaims the address would win over the real
/// one: a `CFLocale` that landed on a dead handle's address would answer the
/// count of somebody else's reference.
///
/// No caller reaches that state today - `refs` is only ever set by
/// [`note_c_owned`], and the owner's `Drop` removes the entry as soon as the last
/// reference goes - so this is the last ungated reader closed rather than a
/// measured failure. It is closed anyway because the cost is three lines and the
/// alternative is leaving exactly one of seven readers trusting a bare address,
/// which is the shape the whole of [`OPAQUE`] exists to refuse.
pub fn c_retain_count(handle: CFTypeRef) -> Option<CFIndex> {
    let entry = raw_entry(handle)?;
    if entry
        .witness
        .is_some_and(|witness| !unsafe { (witness)(handle) })
    {
        return None;
    }
    entry.refs
}

/// Whether the first word of `handle` is a class this runtime built.
///
/// `crate::foreign::is` asks the same question of the same word, but that module
/// only exists on Darwin - there being nowhere else for a foreign CoreFoundation
/// to be met. The word is the same either way, so the question is asked here
/// where it can be asked on every host.
///
/// This is the *check* half of [`is_opaque`], and it is kept separate from it on
/// purpose. A handle in [`OPAQUE`] that begins with a class of ours is a state
/// that cannot exist: nothing in the table is a real Objective-C object, so a
/// first word naming one means the address was freed and handed out again. But
/// "cannot exist" is not "cannot be answered cheaply and safely" - see
/// [`is_opaque`] for why the check is not what decides.
///
/// # Safety
///
/// `handle` must be null or a pointer to at least one readable, aligned word.
pub unsafe fn first_word_is_a_class_of_ours(handle: CFTypeRef) -> bool {
    if handle.is_null() || (handle as usize) % std::mem::align_of::<usize>() != 0 {
        // An unaligned handle is a number this tree invented, not an allocation -
        // `api::tests::an_opaque_cf_handle_uses_its_registered_type_without_an_isa`
        // notes the address of a `u8` on purpose, and reading a word there traps.
        return false;
    }
    // SAFETY: the caller's obligation, as above.
    let isa = unsafe { *(handle as *const *const Class) };
    objc::class::is_known(isa)
}

/// Whether `handle` is in [`OPAQUE`] and its first word is a class of ours, which
/// is the one state the table must never be in.
///
/// Reads one word of a structure whose layout is this crate's business only for
/// *some* of the handles it holds - an `IOHIDElementRef` belongs to `iokit` and a
/// `CGDisplayModeRef` to `appkit` - so this is a check with an answer, not the
/// answer itself. Callers that want the check to *speak* ask for it explicitly
/// rather than having it change behaviour underneath them.
pub fn opaque_collision(handle: CFTypeRef) -> bool {
    if raw_entry(handle).is_none() {
        return false;
    }
    // SAFETY: a handle in the table is a live allocation this tree made, so its
    // first word is readable; the alignment case is handled by the callee.
    unsafe { first_word_is_a_class_of_ours(handle) }
}

/// Whether `handle` is one of them.
///
/// The table is keyed by a bare address, so this cannot be "is this address in
/// the table" - a table entry is a claim about whatever the allocator builds
/// there next. It is therefore the table **and** the owner's witness, and a
/// handle whose owner supplied no witness is answered from the table alone
/// because that owner's allocation cannot die.
///
/// ## Why the class check does not decide this
///
/// The obvious alternative is to ask `first_word_is_a_class_of_ours` here
/// instead of asking the owner, and it is the wrong instrument. Both reasons
/// below were measured in this suite, and the second one is the reason a check
/// is not an instrument at all.
///
/// 1. **It covers only the one collision that is easy to recognise.** A recycled
///    address can be handed to something that is not an object either - a
///    `CFMachPort`, an `IOHIDElementRef`, a `CGDisplayModeRef` - and then neither
///    first word is a class, so the check reports everything is fine while
///    `CFRetain` no-ops and `CFGetTypeID` answers the wrong type. Measured on a
///    receive right allocated by a test: the first word of a `CFMachPort` is
///    `0x7f0000000000`, a Mach port name, and the check is silent for it. The
///    kind registry behind this one ([`crate::runloop::is_noted`]) is the sharper
///    case, because it is keyed on the same address and only ever written by
///    [`crate::runloop::note`]: a `CFMachPort` landing on a dead
///    `CFRunLoopSource`'s address leaves the `Source` entry intact,
///    `crate::api::borrow::<Source>` then rebuilds an `Arc<Source>` over a
///    `MachPort`, and the loop reads that struct's fields as a source's `info`
///    and `perform` and calls the second of them. Measured, not assumed: a
///    *second run-loop handle* at the same address is harmless, because `note`
///    overwrites the entry with its own kind - which is exactly why a test that
///    only checked "is it some other kind" would have been asserting a bug that
///    is not there.
/// 2. **It can only say "definitely not one of ours", and what it cannot say is
///    the whole question.** A detection is not a decision: for every collision
///    the class does not recognise - a zeroed word, a port name, a descriptor, a
///    device index - the check is silent, and a silent check has to be answered
///    *from the table*, which is the bare address this module exists to refuse.
///    So the class check can only ever be a witness that is allowed to say no;
///    it can never be the thing that says yes. That is why it is wired to
///    `ROSETTA3_CHECK_OPAQUE=1` and [`opaque_collision`] rather than to the
///    answer, and it is a better canary than nothing - it is the only instrument
///    here that can catch an owner that never registered a witness at all.
///
/// A witness has neither defect: it is the owner's own word, written once and
/// never again, and - this is the part that is easy to get wrong - its *value*
/// has to be one nothing else in the process can hold. The first version of
/// [`crate::runloop::witness`] required the word to be zero, which excluded the
/// handles this crate hands out and excluded nothing else: a zeroed word is
/// what an unlocked futex, a `bool` and seven bytes of padding, a zeroed
/// `malloc` chunk and a fresh `mmap` page all begin with, so it answered "still
/// mine" for exactly the allocations that land on a freed handle's address -
/// measured, and pinned by
/// `runloop::tests::a_zeroed_word_is_not_a_handle_of_this_modules`. It has the
/// same defect as the class check, one level down, and it is fixed by
/// [`crate::runloop::HANDLE_TAG`] rather than by argument.
///
/// So the class check stays a *named observation* - `ROSETTA3_CHECK_OPAQUE=1`
/// turns it into a refusal that names itself, `ROSETTA3_PROBE_OPAQUE=1` prints
/// the entry's origin - and the answer below is the owner's.
pub fn is_opaque(handle: CFTypeRef) -> bool {
    let Some(entry) = raw_entry(handle) else {
        return false;
    };
    let Some(witness) = entry.witness else {
        return true;
    };
    // SAFETY: the owner registered this witness for this address under the
    // contract in `note_witnessed`, and this is the entry it registered.
    if unsafe { (witness)(handle) } {
        return true;
    }
    // The owner's own witness says this address is not its handle. That is the
    // state the table exists to avoid, detected by the only party that can detect
    // it, and the answer is the one an absent entry gives: this is not a handle
    // of ours, so it is retained, released and type-queried as what it is, which
    // is a real object.
    if check_collisions() {
        objc::fail!(
            "corefoundation: {handle:p} is in the opaque-handle table and the owner's own \
             witness for it does not hold, so the address is not the handle it was noted for - \
             the allocator has handed it out again.  Every type that calls note_typed_opaque \
             must either call forget_opaque when it is destroyed or pass note_witnessed a \
             witness that can tell its own live handles from a recycled address, and a handle \
             the C world owns must reach note_c_owned so a release can free it.  Refusing \
             rather than answering for an object this tree allocated as something it is not."
        );
    }
    #[cfg(test)]
    if std::env::var_os("ROSETTA3_PROBE_OPAQUE").is_some() {
        eprintln!("rosetta3: probe: OPAQUE holds {handle:p}, whose owner's witness does not hold");
        if let Some(from) = provenance_of(handle) {
            eprintln!("rosetta3: probe: {handle:p} was noted here:\n{from}");
        }
        if unsafe { first_word_is_a_class_of_ours(handle) } {
            eprintln!(
                "rosetta3: probe: {handle:p} now begins with a class of ours, so it is a live \
                 object being answered for a handle that has been freed"
            );
        }
    }
    false
}

/// Whether a witness failure should be turned into a refusal rather than an
/// answer.
///
/// Off unless `ROSETTA3_CHECK_OPAQUE=1` is set. The state is meant to be
/// unreachable now that the table asks the owner, and a diagnostic that can kill
/// a run is not a diagnostic - so by default the answer is the safe one (`false`,
/// so the object is treated as the real object it now is) and the refusal is
/// there for whoever is chasing a run where the answer is still wrong.
fn check_collisions() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ROSETTA3_CHECK_OPAQUE").is_some())
}

/// The registered type of an opaque handle, including `Some(0)` for an
/// untyped one.
///
/// Witnessed like [`is_opaque`], and for the same reason: a type read out of an
/// entry whose owner says the address is no longer its handle is a type invented
/// for whatever the allocator built there.
pub fn opaque_type_id(handle: CFTypeRef) -> Option<CFTypeID> {
    match raw_entry(handle) {
        Some(entry) if entry.witness.is_none() => Some(entry.type_id),
        Some(entry) => {
            let witness = entry.witness?;
            // SAFETY: the owner's own function, for the address it was
            // registered for.
            unsafe { (witness)(handle) }.then_some(entry.type_id)
        }
        None => None,
    }
}

/// Each `(Mach-O name, text)` as a cell holding the interned string.
///
/// A pointer-typed global is a *variable*, so the symbol is a cell and the
/// string goes inside it - see [`global_cell`] and [`interned_name`] for the two
/// halves. Seven modules wrote this loop out; the tables they wrote it over are
/// data already, so this is a function rather than anything cleverer.
pub fn string_globals(entries: &[(&str, &str)]) -> Vec<(String, u64)> {
    entries
        .iter()
        .map(|(symbol, text)| (symbol.to_string(), global_cell(symbol, interned_name(text))))
        .collect()
}

/// [`string_globals`] for the globals whose text *is* their name.
///
/// The Mach-O underscore is added here, so the symbol and the string it holds
/// come from one spelling rather than two that could disagree.
pub fn named_string_globals(names: &[&str]) -> Vec<(String, u64)> {
    names
        .iter()
        .map(|name| {
            let symbol = format!("_{name}");
            let cell = global_cell(&symbol, interned_name(name));
            (symbol, cell)
        })
        .collect()
}

/// A constant string global's object, the same one every time it is asked for.
///
/// Five modules had a copy of this, and each had its own table - so the same
/// text reached from two of them was two objects. Identity is the whole point:
/// a guest compares the constant it was handed against the one it imported, and
/// two strings with equal text are not the same string.
///
/// Not [`crate::string::immortal`], which builds a `__CFConstantString`. That is
/// a different class, and `preferences` wants it deliberately; this is the
/// ordinary string every other caller was building.
pub fn interned_name(text: &str) -> u64 {
    static STRINGS: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());
    crate::bootstrap();
    let mut strings = STRINGS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, object)) = strings.iter().find(|(t, _)| t == text) {
        return *object;
    }
    let object = crate::string::from_bytes(text.as_bytes(), false);
    // Retained forever: a global a guest may release is one it can make dangle.
    objc::arc::retain(object as objc::object::Id);
    let address = object as usize as u64;
    strings.push((text.to_string(), address));
    address
}

/// Say once, on stderr, that a call was refused and why.
///
/// Every refusal in this tree names itself, and a refusal that names itself once
/// is better than a value that looks like an answer. `KERN_INVALID_ARGUMENT` and
/// a null `CFTypeRef` are both four bytes and no sentence; a caller that only
/// checks the return - which is what every caller of a `Create` that can fail
/// does - turns one into a log line naming its own file and this runtime's
/// silence. The sentence says what is missing and where it would have to come
/// from, so the next thing to implement is named by the refusal rather than by a
/// debugger session.
///
/// Once per `(what, why)` pair, because these are called from loops - a
/// character-set lookup happens once per identifier per process, but a
/// tokenizer's refusal would be once per token.
pub fn refuse_once(what: &str, why: String) {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static SAID: Mutex<Option<HashSet<(String, String)>>> = Mutex::new(None);
    let key = (what.to_string(), why.clone());
    let fresh = SAID
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_default()
        .insert(key);
    if fresh {
        eprintln!("rosetta3: corefoundation: {what}: refused - {why}");
    }
}

/// The address of the **variable** a pointer-typed global names.
///
/// `kCFRunLoopDefaultMode` is a `CFStringRef` variable, not an object. A guest
/// loads the symbol's address out of its GOT and then loads the object out of
/// *that*, so a symbol defined as the object itself hands the guest the object's
/// `isa` - and `kCFAllocatorDefault`, whose value is null, hands it address
/// zero. Interned by name, because the cell has to be the same one every time a
/// provider is built.
pub fn global_cell(name: &str, value: u64) -> u64 {
    static CELLS: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());
    let mut cells = CELLS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, addr)) = cells.iter().find(|(n, _)| n == name) {
        return *addr;
    }
    // Leaked: the guest holds the address for the life of the process, which is
    // exactly as long as a real framework's `__DATA` word lives.
    let addr = Box::leak(Box::new(value)) as *mut u64 as usize as u64;
    cells.push((name.to_string(), addr));
    addr
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table's whole meaning is "a live handle of this tree", so a note with
    /// no matching forget is a claim about an address that has nothing behind it.
    ///
    /// Asked of *this* handle rather than of the table's size: the table is
    /// process-wide and the suite runs its tests on many threads at once, so a
    /// count is a property of whichever tests happened to be running. That is not a
    /// hypothetical - an earlier version of this test asserted a count and failed
    /// in 11 runs of 60 for exactly that reason.
    #[test]
    fn a_note_with_a_forget_leaves_nothing_behind() {
        crate::bootstrap();
        static HANDLE: u64 = 0;
        let handle = std::ptr::addr_of!(HANDLE).cast::<c_void>();
        note_typed_opaque(handle, 0xfeed);
        assert!(is_opaque(handle), "a note is an entry");
        assert_eq!(opaque_type_id(handle), Some(0xfeed));
        forget_opaque(handle);
        assert!(!is_opaque(handle), "a forget is its pair");
        assert_eq!(opaque_type_id(handle), None);
        // Idempotent, because a handle's `Drop` and a caller's cleanup can both
        // reach it and neither can know about the other.
        forget_opaque(handle);
        assert!(!is_opaque(handle));
    }

    /// Not one reader of the table may answer for an address its owner disclaims.
    ///
    /// The table is keyed by a bare address, and every question asked through it
    /// is therefore a question about *whatever the allocator built there*. Seven
    /// functions read it, and the defect this module exists to refuse is a reader
    /// that trusts the key on its own - so the invariant is a property of the set
    /// and not of any one function, and a reader added later is covered by
    /// pinning all of them here.
    ///
    /// The state is built by hand because the owner's `Drop` makes it
    /// unreachable: a counted entry is only ever created by [`note_c_owned`], and
    /// the last `release` fires the owner's `Drop`, which forgets the address. So
    /// this is a statement about what every reader *would* answer, not a
    /// reproduction - and the release function below is written to fail loudly, so
    /// a reader that fell through to it would not pass quietly.
    #[test]
    fn no_reader_of_the_table_answers_for_an_address_its_owner_disclaims() {
        crate::bootstrap();
        // An owner that says "this is not my handle" for any address, which is the
        // question `witness` answers when the allocation at a noted address has
        // been freed and handed out again.
        unsafe fn disclaims(_handle: CFTypeRef) -> bool {
            false
        }
        static FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        unsafe fn release(_handle: CFTypeRef) {
            FIRED.store(true, std::sync::atomic::Ordering::Release);
        }
        let object = crate::string::from_bytes(b"a string that is an object", false);
        note_witnessed(object, 0, Some(disclaims));
        note_c_owned(object, release);
        // The state, stated first: the table really does hold a counted entry here,
        // so every answer below is the reader's own and not an absent entry.
        assert_eq!(raw_entry(object).map(|entry| entry.refs), Some(Some(1)));

        assert!(!is_opaque(object), "is_opaque");
        assert!(!witnessed(object), "witnessed");
        assert_eq!(opaque_type_id(object), None, "opaque_type_id");
        assert_eq!(retain_opaque(object), CRef::Unknown, "retain_opaque");
        assert_eq!(release_opaque(object), CRef::Unknown, "release_opaque");
        assert!(!spent(object), "spent");
        // The one that did not, and the reason `CFGetRetainCount` asks this before
        // it asks `is_opaque`: a count read out of a disclaimed entry is a
        // reference count invented for whatever the allocator built there.
        assert_eq!(
            c_retain_count(object),
            None,
            "c_retain_count must not answer for an address its owner disclaims either - it is the \
             one reader CFGetRetainCount asks first, so a count left here would win over the real \
             one"
        );
        assert!(
            !FIRED.load(std::sync::atomic::Ordering::Acquire),
            "and no reader may reach the owner's release for a handle it has disclaimed"
        );
        forget_opaque(object);
        // SAFETY: the one reference this test made.
        unsafe { crate::api::CFRelease(object) };
    }

    /// The collision check, pinned as a check.
    ///
    /// Nothing in the table is a real Objective-C object, so an entry whose
    /// address now begins with a class of ours is the state
    /// `ROSETTA3_CHECK_OPAQUE=1` refuses by name and the probe prints. It is
    /// *detected* here rather than acted on: the decision to let the pairing with
    /// the allocation carry the correctness is argued in [`is_opaque`], and a test
    /// that asserted the opposite would be asserting a policy rather than a fact.
    /// The fact is that the check fires on exactly this state and not on a real
    /// handle - which is the half that would make it safe to trust if it were
    /// load-bearing, and it is why it is not.
    #[test]
    fn the_collision_check_fires_on_a_live_object_and_not_on_a_handle() {
        crate::bootstrap();
        // The positive control: a real object of this runtime's is recognised.
        let object = crate::string::from_bytes(b"a string that is an object", false);
        // SAFETY: an object this test just made, so its first word is its isa.
        assert!(unsafe { first_word_is_a_class_of_ours(object) });
        // And the check is silent for a handle, whose first word is a flag.
        let source = crate::runloop::new_source(None, 0);
        // SAFETY: a handle this module made, so its first word is readable.
        assert!(!unsafe {
            first_word_is_a_class_of_ours(std::sync::Arc::as_ptr(&source).cast())
        });
        // Now the impossible state, made the only way it can be made by hand: an
        // entry for the address of a real object.
        static HANDLE: u64 = 0;
        let handle = std::ptr::addr_of!(HANDLE).cast::<c_void>();
        note_typed_opaque(handle, 0xfeed);
        assert!(opaque_collision(handle) == false, "a u8 is not an object");
        forget_opaque(handle);
        note_typed_opaque(object, 0xfeed);
        assert!(
            opaque_collision(object),
            "an entry over a live object is the state the check exists to name"
        );
        assert!(
            is_opaque(object),
            "and the default answer is still the table's, which is what the pairing is for"
        );
        forget_opaque(object);
        // SAFETY: the object this test made, and nothing else holds a reference.
        unsafe { crate::api::CFRelease(object) };
    }
}
