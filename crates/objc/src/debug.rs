use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::class::Class;
use crate::object::Id;

const GUARD_BYTES: usize = 32;
const GUARD: u8 = 0xa5;
const DEAD: u8 = 0xdd;

static ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Live,
    Deallocating,
    Dead,
}

struct Allocation {
    class: usize,
    kind: &'static str,
    size: usize,
    state: State,
    origin: &'static std::panic::Location<'static>,
}

static OBJECTS: LazyLock<Mutex<HashMap<usize, Allocation>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static HANDLES: LazyLock<Mutex<HashSet<(usize, &'static str)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub(crate) fn note_handle<T>(handle: *const T, kind: &'static str) {
    if !enabled() {
        return;
    }
    HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert((handle as usize, kind));
}

pub(crate) fn validate_handle<T>(handle: *const T, kind: &'static str, operation: &str) {
    if enabled()
        && !handle.is_null()
        && !HANDLES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(handle as usize, kind))
    {
        violation(
            operation,
            handle as *mut c_void,
            &format!("invalid {kind} handle"),
        );
    }
}

/// Enable Objective-C hardening for every subsequently allocated object.
///
/// This must be called before Objective-C bootstrap. It is deliberately
/// irreversible: changing allocation rules halfway through a process would
/// make the absence of a record ambiguous.
pub fn enable() {
    ENABLED.store(true, Ordering::Release);
}

/// Whether Objective-C hardening is enabled.
#[inline]
pub fn enabled() -> bool {
    if ENABLED.load(Ordering::Acquire) {
        return true;
    }
    static REQUESTED_AT_LAUNCH: LazyLock<bool> =
        LazyLock::new(|| requested_at_launch(std::env::args_os().skip(1)));
    if *REQUESTED_AT_LAUNCH {
        ENABLED.store(true, Ordering::Release);
        return true;
    }
    false
}

fn requested_at_launch(mut arguments: impl Iterator<Item = std::ffi::OsString>) -> bool {
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("run")) {
        return false;
    }
    for argument in arguments {
        match argument.to_str() {
            Some("--objc-debug") => return true,
            Some("--slow" | "--native" | "--emulated") => {}
            // This is the guest path. Everything after it belongs to the
            // guest, including an argument with the same spelling.
            _ => return false,
        }
    }
    false
}

/// The allocation size needed for an object with `size` usable bytes.
pub(crate) fn allocation_size(size: usize) -> Option<usize> {
    match enabled() {
        true => size.checked_add(GUARD_BYTES),
        false => Some(size),
    }
}

/// Record a freshly initialized object and write its tail guard.
///
/// # Safety
///
/// `object` points to an allocation of [`allocation_size`] bytes.
#[track_caller]
pub(crate) unsafe fn allocated(object: Id, class: &'static Class, size: usize) {
    if !enabled() || object.is_null() {
        return;
    }
    // SAFETY: the caller allocated the guarded size.
    unsafe { std::ptr::write_bytes((object as *mut u8).add(size), GUARD, GUARD_BYTES) };
    let previous = OBJECTS.lock().unwrap_or_else(|e| e.into_inner()).insert(
        object as usize,
        Allocation {
            class: crate::class::ptr(class) as usize,
            kind: "object",
            size,
            state: State::Live,
            origin: std::panic::Location::caller(),
        },
    );
    if previous.is_some() {
        violation(
            "allocation",
            object,
            "the allocator reused a quarantined object address",
        );
    }
}

/// Record guarded runtime storage which is reference counted like an object
/// but has no Objective-C class, such as a copied `__block` variable.
///
/// # Safety
///
/// `pointer` points to an allocation of [`allocation_size`] bytes.
#[track_caller]
pub(crate) unsafe fn allocated_raw(pointer: *mut c_void, size: usize, kind: &'static str) {
    if !enabled() || pointer.is_null() {
        return;
    }
    // SAFETY: the caller allocated the guarded size.
    unsafe { std::ptr::write_bytes((pointer as *mut u8).add(size), GUARD, GUARD_BYTES) };
    let previous = OBJECTS.lock().unwrap_or_else(|e| e.into_inner()).insert(
        pointer as usize,
        Allocation {
            class: 0,
            kind,
            size,
            state: State::Live,
            origin: std::panic::Location::caller(),
        },
    );
    if previous.is_some() {
        violation(
            "allocation",
            pointer,
            "the allocator reused a quarantined address",
        );
    }
}

fn guard_is_intact(object: Id, allocation: &Allocation) -> bool {
    // SAFETY: debug allocations remain quarantined and include the guard.
    let guard = unsafe {
        std::slice::from_raw_parts((object as *const u8).add(allocation.size), GUARD_BYTES)
    };
    guard.iter().all(|byte| *byte == GUARD)
}

fn allocation_name(allocation: &Allocation) -> &'static str {
    if allocation.class == 0 {
        return allocation.kind;
    }
    // SAFETY: allocation records only store leaked classes.
    unsafe { &*(allocation.class as *const Class) }.name_str()
}

fn violation(operation: &str, object: Id, reason: &str) -> ! {
    eprintln!("rosemary: objc debug: {operation}({object:p}): {reason}");
    GUEST_CONTEXT.with(|context| {
        if let Some((pc, symbol)) = context.borrow().last() {
            eprintln!("rosemary: objc debug: guest {symbol} at {pc:#x}");
        }
    });
    std::process::abort()
}

/// Validate a managed object before an operation reads it.
///
/// Unknown objects are admitted. Guest constant objects and storage passed to
/// `objc_constructInstance` are not allocated by this runtime, but their `isa`
/// is validated separately by the ordinary class lookup.
pub(crate) fn validate(object: Id, operation: &str) {
    if !enabled() || object.is_null() {
        return;
    }
    let objects = OBJECTS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(allocation) = objects.get(&(object as usize)) else {
        drop(objects);
        if !(object as usize).is_multiple_of(std::mem::align_of::<*const Class>()) {
            violation(operation, object, "object pointer is not aligned");
        }
        // SAFETY: Objective-C API callers promise a readable object pointer.
        // External guest allocations are accepted only when their first word
        // identifies a class belonging to this runtime.
        let isa = unsafe { *(object as *const *const Class) };
        if !crate::class::is_known(isa) {
            violation(operation, object, "object has an invalid isa");
        }
        return;
    };
    match allocation.state {
        State::Dead => violation(
            operation,
            object,
            &format!(
                "use of finalized {} allocated at {}:{}",
                allocation_name(allocation),
                allocation.origin.file(),
                allocation.origin.line()
            ),
        ),
        State::Live | State::Deallocating => {}
    }
    if !guard_is_intact(object, allocation) {
        violation(
            operation,
            object,
            &format!(
                "tail guard for {} was overwritten past its {}-byte allocation",
                allocation_name(allocation),
                allocation.size
            ),
        );
    }
}

thread_local! {
    static GUEST_CONTEXT: std::cell::RefCell<Vec<(u64, String)>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

/// A scoped annotation identifying a checked operation as guest-originated.
pub struct GuestContext {
    active: bool,
}

impl Drop for GuestContext {
    fn drop(&mut self) {
        if self.active {
            GUEST_CONTEXT.with(|context| {
                context.borrow_mut().pop();
            });
        }
    }
}

/// Attach a guest program counter and imported symbol to diagnostics produced
/// while servicing one guest operation.
pub fn guest_context(pc: u64, symbol: &str) -> GuestContext {
    let active = enabled();
    if active {
        GUEST_CONTEXT.with(|context| context.borrow_mut().push((pc, symbol.to_owned())));
    }
    GuestContext { active }
}

/// Mark the last-release transition before any destructor can run.
pub(crate) fn begin_deallocation(object: Id) {
    if !enabled() || object.is_null() {
        return;
    }
    validate(object, "release");
    let mut objects = OBJECTS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(allocation) = objects.get_mut(&(object as usize)) else {
        return;
    };
    if allocation.state != State::Live {
        violation("release", object, "object is already being deallocated");
    }
    allocation.state = State::Deallocating;
}

pub(crate) fn set_class(object: Id, class: &'static Class) {
    if !enabled() || object.is_null() {
        return;
    }
    validate(object, "object_setClass");
    let mut objects = OBJECTS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(allocation) = objects.get_mut(&(object as usize)) else {
        return;
    };
    if class.instance_size() > allocation.size {
        violation(
            "object_setClass",
            object,
            &format!(
                "{} needs {} bytes but this allocation has {}",
                class.name_str(),
                class.instance_size(),
                allocation.size
            ),
        );
    }
    allocation.class = crate::class::ptr(class) as usize;
}

pub(crate) fn validate_range(object: Id, offset: isize, width: usize, operation: &str) {
    if !enabled() || object.is_null() {
        return;
    }
    validate(object, operation);
    let objects = OBJECTS.lock().unwrap_or_else(|e| e.into_inner());
    let size = match objects.get(&(object as usize)) {
        Some(allocation) => allocation.size,
        None => {
            drop(objects);
            // SAFETY: unknown external objects are admitted under the normal
            // rule, and their known isa supplies the declared instance size.
            unsafe { crate::object::class_of(object) }.map_or(0, Class::instance_size)
        }
    };
    let Some(end) = usize::try_from(offset)
        .ok()
        .and_then(|start| start.checked_add(width))
    else {
        violation(operation, object, "negative or overflowing object offset");
    };
    if end > size {
        violation(
            operation,
            object,
            &format!("access ends at byte {end}, past the {size}-byte object"),
        );
    }
}

/// Finalize and quarantine an allocation. Returns whether normal `free` must
/// be skipped.
pub(crate) fn finalized(object: Id) -> bool {
    if !enabled() || object.is_null() {
        return false;
    }
    validate(object, "finalize");
    let mut objects = OBJECTS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(allocation) = objects.get_mut(&(object as usize)) else {
        return false;
    };
    if allocation.state != State::Deallocating {
        violation(
            "finalize",
            object,
            "object was finalized outside its dealloc chain",
        );
    }
    allocation.state = State::Dead;
    // SAFETY: the allocation is deliberately retained in quarantine.
    unsafe { std::ptr::write_bytes(object as *mut u8, DEAD, allocation.size) };
    true
}

/// Require a custom `-dealloc` implementation to end at `NSObject`'s
/// finalizer.
pub(crate) fn require_finalized(object: Id) {
    if !enabled() || object.is_null() {
        return;
    }
    let objects = OBJECTS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(allocation) = objects.get(&(object as usize)) else {
        return;
    };
    if allocation.state != State::Dead {
        violation("dealloc", object, "-dealloc returned without calling super");
    }
}

static NEXT_POOL: AtomicUsize = AtomicUsize::new(0x10000);
static POOLS: LazyLock<Mutex<HashMap<usize, (u64, bool)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn new_pool(thread: u64) -> *mut c_void {
    let token = NEXT_POOL.fetch_add(1, Ordering::Relaxed);
    POOLS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(token, (thread, true));
    token as *mut c_void
}

pub(crate) fn validate_pool(token: *mut c_void, thread: u64) {
    let pools = POOLS.lock().unwrap_or_else(|e| e.into_inner());
    match pools.get(&(token as usize)) {
        None => violation("objc_autoreleasePoolPop", token, "unknown pool token"),
        Some((_, false)) => violation("objc_autoreleasePoolPop", token, "pool was already popped"),
        Some((owner, true)) if *owner != thread => violation(
            "objc_autoreleasePoolPop",
            token,
            "pool belongs to another thread",
        ),
        Some(_) => {}
    }
}

pub(crate) fn pool_popped(token: *mut c_void) {
    if let Some((_, active)) = POOLS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&(token as usize))
    {
        *active = false;
    }
}

/// Every weak slot the runtime has a registration for, and what it points at.
///
/// A slot with no entry is not automatically a fault: see [`require_zeroed`].
static WEAK_SLOTS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Accept an unregistered slot only when it reads as `nil`.
///
/// A weak slot is a word of storage, not a handle: `objc_storeWeak`,
/// `objc_loadWeak*` and `objc_destroyWeak` are defined on any zeroed word,
/// with no `objc_initWeak` before them. Two ordinary shapes rely on it - a
/// `__weak` ivar or global, which is born zeroed and is never initialized
/// explicitly, and the source slot of `objc_moveWeak`, which is left zeroed
/// and unregistered and is then destroyed again by the scope cleanup that
/// clang emits for the moved-from variable.
///
/// What is a fault is a *non-nil* word with no registration: uninitialized
/// storage handed to the runtime, or a live weak slot copied bytewise out
/// from under it.
///
/// # Safety
///
/// `slot` is a readable word owned by the caller.
unsafe fn require_zeroed(slot: *mut Id, operation: &str) {
    // SAFETY: the caller owns `slot`.
    if !unsafe { *slot }.is_null() {
        violation(operation, slot.cast(), "weak slot was never initialized");
    }
}

pub(crate) fn weak_init(slot: *mut Id, object: Id) {
    if !enabled() {
        return;
    }
    validate(object, "objc_initWeak");
    let mut slots = WEAK_SLOTS.lock().unwrap_or_else(|e| e.into_inner());
    if slots.insert(slot as usize, object as usize).is_some() {
        violation(
            "objc_initWeak",
            slot.cast(),
            "weak slot is already initialized",
        );
    }
}

pub(crate) fn weak_store(slot: *mut Id, object: Id) {
    if !enabled() {
        return;
    }
    validate(object, "objc_storeWeak");
    let mut slots = WEAK_SLOTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(value) = slots.get_mut(&(slot as usize)) {
        *value = object as usize;
        return;
    }
    drop(slots);
    // SAFETY: `objc_storeWeak` promises a readable word.
    unsafe { require_zeroed(slot, "objc_storeWeak") };
    WEAK_SLOTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(slot as usize, object as usize);
}

pub(crate) fn weak_load(slot: *mut Id, operation: &str) {
    if !enabled()
        || WEAK_SLOTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&(slot as usize))
    {
        return;
    }
    // SAFETY: a weak load promises a readable word.
    unsafe { require_zeroed(slot, operation) };
}

pub(crate) fn weak_destroy(slot: *mut Id) {
    if !enabled()
        || WEAK_SLOTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(slot as usize))
            .is_some()
    {
        return;
    }
    // SAFETY: `objc_destroyWeak` promises a readable word.
    unsafe { require_zeroed(slot, "objc_destroyWeak") };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class::Imp;

    const CHILD: &str = "ROSEMARY_OBJC_DEBUG_TEST";

    fn arguments<'a>(values: &'a [&'a str]) -> impl Iterator<Item = std::ffi::OsString> + 'a {
        values.iter().map(std::ffi::OsString::from)
    }

    #[test]
    fn only_a_driver_option_before_the_guest_path_enables_hardening() {
        assert!(requested_at_launch(arguments(&[
            "run",
            "--emulated",
            "--objc-debug",
            "guest"
        ])));
        assert!(!requested_at_launch(arguments(&[
            "run",
            "guest",
            "--objc-debug"
        ])));
        assert!(!requested_at_launch(arguments(&["guest", "--objc-debug"])));
    }

    unsafe extern "C" fn incomplete_dealloc(_object: Id, _sel: crate::sel::Sel) {}

    fn object(class_name: &std::ffi::CStr, dealloc: Option<Imp>) -> Id {
        crate::bootstrap();
        let root = crate::class::lookup(c"NSObject").expect("NSObject");
        let class = crate::class::allocate_pair(Some(root), class_name, 0).expect("fresh class");
        if let Some(imp) = dealloc {
            assert!(class.add_method(crate::sel::get("dealloc"), imp, c"v16@0:8"));
        }
        crate::class::register_pair(class);
        crate::object::create_instance(class, 0)
    }

    fn child(case: &str) {
        enable();
        match case {
            "valid" => {
                let object = object(c"RosemaryDebugValid", None);
                assert_eq!(crate::arc::retain(object), object);
                crate::arc::release(object);
                crate::arc::release(object);
            }
            // What clang emits for a `__weak` local captured by value: the
            // slot is moved out of, and the scope cleanup destroys it anyway.
            "moved-weak" => {
                let object = object(c"RosemaryDebugMovedWeak", None);
                let (mut from, mut to) = (std::ptr::null_mut(), std::ptr::null_mut());
                // SAFETY: two words of this frame's own storage.
                unsafe {
                    crate::api::objc_initWeak(&mut from, object);
                    crate::api::objc_moveWeak(&mut to, &mut from);
                    crate::api::objc_destroyWeak(&mut from);
                    crate::api::objc_destroyWeak(&mut to);
                }
            }
            // A `__weak` ivar is born zeroed and never sees `objc_initWeak`.
            "zeroed-weak" => {
                let object = object(c"RosemaryDebugZeroedWeak", None);
                let mut ivar = std::ptr::null_mut();
                // SAFETY: one word of this frame's own storage.
                unsafe {
                    crate::api::objc_storeWeak(&mut ivar, object);
                    crate::api::objc_loadWeakRetained(&mut ivar);
                    crate::api::objc_destroyWeak(&mut ivar);
                }
            }
            "guard" => {
                let object = object(c"RosemaryDebugGuard", None);
                // SAFETY: deliberately corrupt the first debug-only guard byte.
                unsafe {
                    let class = crate::object::class_of(object).expect("class");
                    (object as *mut u8).add(class.instance_size()).write(0);
                }
                crate::arc::retain(object);
            }
            "overrelease" => {
                let object = object(c"RosemaryDebugOverrelease", None);
                crate::arc::release(object);
                crate::arc::release(object);
            }
            "exit-pool" => {
                struct Teardown;
                impl Drop for Teardown {
                    fn drop(&mut self) {
                        let pool = crate::arc::pool_push();
                        crate::arc::pool_pop(pool);
                    }
                }
                thread_local! {
                    static TEARDOWN: Teardown = const { Teardown };
                }
                std::thread::spawn(|| {
                    // Registered before the pools, so destroyed after them.
                    TEARDOWN.with(|_| {});
                    let pool = crate::arc::pool_push();
                    crate::arc::pool_pop(pool);
                })
                .join()
                .expect("a pool pushed and popped at thread exit is a no-op");
            }
            "dealloc" => {
                let object = object(c"RosemaryDebugDealloc", Some(incomplete_dealloc as Imp));
                crate::arc::release(object);
            }
            "pool" => {
                let pool = crate::arc::pool_push();
                crate::arc::pool_pop(pool);
                crate::arc::pool_pop(pool);
            }
            "weak" => {
                // A zeroed slot is legal storage for `objc_storeWeak`; garbage
                // in the slot is what the check is for.
                let mut slot = std::ptr::null_mut();
                crate::arc::store_weak(&mut slot, std::ptr::null_mut());
                crate::arc::destroy_weak(&mut slot);
                let mut garbage = 0x10usize as Id;
                crate::arc::destroy_weak(&mut garbage);
            }
            "property" => {
                let object = object(c"RosemaryDebugProperty", None);
                // SAFETY: deliberately supplies an offset outside the object.
                unsafe {
                    crate::shorthand::objc_getProperty(
                        object,
                        crate::sel::get("value"),
                        4096,
                        false,
                    );
                }
            }
            "class" => {
                // SAFETY: deliberately supplies an invalid class handle.
                unsafe { crate::api::class_getName(0x10usize as *const c_void) };
            }
            "selector" => {
                let object = object(c"RosemaryDebugSelector", None);
                // SAFETY: deliberately supplies an invalid selector handle.
                unsafe {
                    crate::object::class_of(object)
                        .expect("class")
                        .method(0x10usize as crate::sel::Sel);
                }
            }
            "sync" => {
                let object = object(c"RosemaryDebugSync", None);
                crate::arc::sync_exit(object);
            }
            "external" => {
                crate::bootstrap();
                let mut storage =
                    crate::class::ptr(crate::class::lookup(c"NSObject").expect("NSObject"));
                validate((&raw mut storage).cast(), "external object test");
            }
            "isa" => {
                let mut storage: *mut Class = std::ptr::null_mut();
                validate((&raw mut storage).cast(), "invalid isa test");
            }
            _ => panic!("unknown child case"),
        }
    }

    #[test]
    fn hardening_accepts_valid_lifetimes_and_aborts_on_contract_violations() {
        if let Ok(case) = std::env::var(CHILD) {
            child(&case);
            return;
        }
        let executable = std::env::current_exe().expect("current test executable");
        for (case, expected) in [
            ("valid", None),
            ("moved-weak", None),
            ("zeroed-weak", None),
            ("guard", Some("tail guard")),
            ("overrelease", Some("use of finalized")),
            ("dealloc", Some("without calling super")),
            ("pool", Some("already popped")),
            ("exit-pool", None),
            ("weak", Some("weak slot was never initialized")),
            ("property", Some("past the")),
            ("class", Some("invalid class handle")),
            ("selector", Some("invalid selector handle")),
            ("sync", Some("without a matching enter")),
            ("external", None),
            ("isa", Some("invalid isa")),
        ] {
            let output = std::process::Command::new(&executable)
                .args([
                    "--exact",
                    "debug::tests::hardening_accepts_valid_lifetimes_and_aborts_on_contract_violations",
                    "--nocapture",
                ])
                .env(CHILD, case)
                .output()
                .expect("run debug child");
            let stderr = String::from_utf8_lossy(&output.stderr);
            match expected {
                None => assert!(output.status.success(), "{case}: {stderr}"),
                Some(message) => {
                    assert!(!output.status.success(), "{case} unexpectedly survived");
                    assert!(stderr.contains(message), "{case}: {stderr}");
                }
            }
        }
    }
}
