use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{LazyLock, Mutex, MutexGuard, OnceLock, RwLock};

use crate::address::AddressMap;
use crate::object::Id;

const SHARDS: usize = 64;

static COUNTS: [Mutex<AddressMap<usize>>; SHARDS] =
    [const { Mutex::new(HashMap::with_hasher(std::hash::BuildHasherDefault::new())) }; SHARDS];

fn counts(object: Id) -> MutexGuard<'static, AddressMap<usize>> {
    let shard = (crate::address::mix(object as u64) >> 58) as usize % SHARDS;
    COUNTS[shard].lock().unwrap_or_else(|e| e.into_inner())
}

/// How an `OS_object` counts references: `(retain, release, count)`.
///
/// A dispatch object keeps its count in its own header, where `dispatch_retain`
/// reads it. Counting ARC's references here instead would split one object's
/// count in two, and a release that finds nothing here frees the object - even
/// the main queue, which must outlive the process.
pub struct OsObjectRefcount {
    pub retain: fn(Id),
    pub release: fn(Id),
    pub count: fn(Id) -> usize,
}

static OS_OBJECT_REFCOUNT: OnceLock<OsObjectRefcount> = OnceLock::new();

/// Install the refcount `OS_object`s use. The first installation wins.
pub fn set_os_object_refcount(refcount: OsObjectRefcount) {
    let _ = OS_OBJECT_REFCOUNT.set(refcount);
}

/// The refcount `object` uses, if it is an `OS_object`.
fn os_object_refcount(object: Id) -> Option<&'static OsObjectRefcount> {
    let refcount = OS_OBJECT_REFCOUNT.get()?;
    // SAFETY: a live object, whose first word is the `isa`.
    let class = unsafe { crate::object::class_of(object) }?;
    std::ptr::eq(class, crate::class::os_object_class()).then_some(refcount)
}

/// Add a reference. Returns `object`, as `objc_retain` does.
pub fn retain(object: Id) -> Id {
    if object.is_null() {
        return object;
    }
    crate::debug::validate(object, "retain");
    if crate::class::is_known(object as *const crate::class::Class) {
        return object;
    }
    // A block counts its own references, in its `flags` word: see
    // `crate::block`. Retaining one here would leave the count in the block
    // untouched and a stack block on the stack.
    // SAFETY: a live object, whose first word is the `isa`.
    if unsafe { crate::block::is_block(object) } {
        return crate::block::copy(object);
    }
    if let Some(refcount) = os_object_refcount(object) {
        (refcount.retain)(object);
        return object;
    }
    let mut map = counts(object);
    let extra = map.entry(object as usize).or_insert(0);
    if crate::debug::enabled() {
        *extra = extra
            .checked_add(1)
            .unwrap_or_else(|| crate::fail!("objc debug: retain count overflow for {object:p}"));
    } else {
        *extra += 1;
    }
    object
}

/// Pin `object` for the life of the process: no number of releases frees it.
///
/// For singletons a guest is handed unowned and may over-release. Blocks and
/// `OS_object`s keep their counts elsewhere, where a pin here would not reach.
pub fn make_immortal(object: Id) {
    if object.is_null() || crate::class::is_known(object as *const crate::class::Class) {
        return;
    }
    // SAFETY: a live object; see [`retain`].
    if unsafe { crate::block::is_block(object) } || os_object_refcount(object).is_some() {
        crate::fail!("make_immortal: {object:p} does not use this runtime's refcount");
    }
    counts(object).insert(object as usize, usize::MAX / 2);
}

/// Drop a reference, deallocating at zero.
///
/// Deallocation is `-dealloc` if the class implements it, and a plain `free`
/// otherwise - the same order Apple's runtime uses, and the reason a class that
/// owns memory gets a chance to give it back.
pub fn release(object: Id) {
    if object.is_null() {
        return;
    }
    crate::debug::validate(object, "release");
    if crate::class::is_known(object as *const crate::class::Class) {
        return;
    }
    // SAFETY: a live object; see [`retain`].
    if unsafe { crate::block::is_block(object) } {
        return crate::block::release(object);
    }
    if let Some(refcount) = os_object_refcount(object) {
        return (refcount.release)(object);
    }
    {
        let mut map = counts(object);
        if let Some(extra) = map.get_mut(&(object as usize)) {
            *extra -= 1;
            if *extra == 0 {
                map.remove(&(object as usize));
            }
            return;
        }
    }
    deallocate(object);
}

/// The current reference count, as `-retainCount` reports it.
pub fn retain_count(object: Id) -> usize {
    if object.is_null() {
        return 0;
    }
    crate::debug::validate(object, "retainCount");
    if crate::class::is_known(object as *const crate::class::Class) {
        return usize::MAX;
    }
    if let Some(refcount) = os_object_refcount(object) {
        return (refcount.count)(object);
    }
    1 + counts(object).get(&(object as usize)).copied().unwrap_or(0)
}

/// Send `-dealloc` if there is one, then give the memory back.
fn deallocate(object: Id) {
    crate::debug::begin_deallocation(object);
    clear_weak_references(object);
    let dealloc = crate::sel::get("dealloc");
    // SAFETY: `object` is an object this runtime allocated.
    if let Some(class) = unsafe { crate::object::class_of(object) }
        && let Some(method) = class.method(dealloc)
    {
        let imp = method.implementation();
        // SAFETY: `-dealloc` is `void (*)(id, SEL)` in every Objective-C there
        // has ever been.
        let f: unsafe extern "C" fn(Id, crate::sel::Sel) = unsafe { std::mem::transmute(imp) };
        // SAFETY: as above. An override is expected to end by calling super's,
        // and `NSObject`'s is what frees.
        unsafe { f(object, dealloc) };
        crate::debug::require_finalized(object);
        return;
    }
    // SAFETY: nothing holds a reference; see `release`.
    unsafe { crate::object::dispose_instance(object) };
}

/// Free an object whose `-dealloc` chain has finished. `NSObject`'s `-dealloc`.
pub fn finalize(object: Id) {
    // SAFETY: reached only from the end of a `-dealloc` chain.
    unsafe { crate::object::dispose_instance(object) }
}

thread_local! {
    /// The autorelease stack for this thread: one `Vec` of objects per pool.
    static POOLS: std::cell::RefCell<Vec<Pool>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct Pool {
    token: *mut c_void,
    objects: Vec<Id>,
}

/// The sentinel `objc_autoreleasePoolPop` is given back.
///
/// The depth of the pool that was pushed, plus one so it is never null. Apple
/// returns an interior pointer; nothing may read it either way.
/// The token of a pool pushed after this thread's pools were destroyed: a
/// destructor running at thread exit gets one, and popping it does nothing.
/// No real token is this value.
const DEAD_POOL: *mut c_void = usize::MAX as *mut c_void;

pub fn pool_push() -> *mut c_void {
    POOLS
        .try_with(|pools| {
            let mut pools = pools.borrow_mut();
            let token = if crate::debug::enabled() {
                crate::debug::new_pool(thread_id())
            } else {
                (pools.len() + 1) as *mut c_void
            };
            pools.push(Pool {
                token,
                objects: Vec::new(),
            });
            token
        })
        .unwrap_or(DEAD_POOL)
}

/// Release everything added since the matching [`pool_push`].
///
/// Pools nest, so a token naming an outer pool pops the inner ones too - which
/// is what happens when a `@autoreleasepool` unwinds past one.
pub fn pool_pop(token: *mut c_void) {
    if token == DEAD_POOL {
        return;
    }
    if crate::debug::enabled() {
        let Ok(found) =
            POOLS.try_with(|pools| pools.borrow().iter().any(|pool| pool.token == token))
        else {
            return;
        };
        crate::debug::validate_pool(token, thread_id());
        if !found {
            crate::fail!(
                "objc debug: autorelease pool token {token:p} is not on this thread's stack"
            );
        }
    }
    loop {
        let Ok(top) = POOLS.try_with(|pools| pools.borrow_mut().pop()) else {
            return;
        };
        let Some(pool) = top else {
            if crate::debug::enabled() {
                crate::fail!("objc debug: autorelease pool pop with an empty pool stack");
            }
            return;
        };
        for object in pool.objects {
            release(object);
        }
        if crate::debug::enabled() {
            crate::debug::pool_popped(pool.token);
        }
        if pool.token == token {
            return;
        }
    }
}

/// Hand `object` to the innermost pool. Returns `object`.
///
/// With no pool at all the object is leaked rather than released, and said so
/// once: releasing it here would be a use-after-free in the caller, which is
/// strictly worse than a leak.
pub fn autorelease(object: Id) -> Id {
    if object.is_null() {
        return object;
    }
    crate::debug::validate(object, "autorelease");
    let pushed = POOLS.try_with(|pools| match pools.borrow_mut().last_mut() {
        Some(pool) => pool.objects.push(object),
        None => no_pool(),
    });
    if pushed.is_err() {
        no_pool();
    }
    object
}

fn no_pool() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SAID: AtomicBool = AtomicBool::new(false);
    if !SAID.swap(true, Ordering::Relaxed) {
        eprintln!("rosemary: objc: autorelease with no pool in place; leaking");
    }
}

/// Every live `__weak` slot, by the object it points at.
static WEAK: LazyLock<RwLock<HashMap<usize, Vec<usize>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Point `slot` at `object`, registering it to be zeroed on deallocation.
pub fn store_weak(slot: *mut Id, object: Id) -> Id {
    crate::debug::weak_store(slot, object);
    unregister_weak(slot);
    // SAFETY: the caller owns `slot`, which is a word of its own storage.
    unsafe { *slot = object };
    if !object.is_null() {
        WEAK.write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(object as usize)
            .or_default()
            .push(slot as usize);
    }
    object
}

/// The object in `slot`, retained and autoreleased so the caller can use it.
pub fn load_weak_retained(slot: *mut Id) -> Id {
    crate::debug::weak_load(slot, "objc_loadWeakRetained");
    // SAFETY: the caller owns `slot`.
    let object = unsafe { *slot };
    if object.is_null() {
        return object;
    }
    retain(object)
}

/// Unregister `slot` and clear it.
pub fn destroy_weak(slot: *mut Id) {
    crate::debug::weak_destroy(slot);
    unregister_weak(slot);
}

/// Initialize uninitialized weak storage.
pub fn init_weak(slot: *mut Id, object: Id) -> Id {
    crate::debug::weak_init(slot, object);
    // SAFETY: the caller supplies a writable word of uninitialized storage.
    unsafe { *slot = object };
    if !object.is_null() {
        WEAK.write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(object as usize)
            .or_default()
            .push(slot as usize);
    }
    object
}

fn unregister_weak(slot: *mut Id) {
    // SAFETY: the caller owns `slot`.
    let object = unsafe { *slot };
    if object.is_null() {
        return;
    }
    let mut guard = WEAK.write().unwrap_or_else(|e| e.into_inner());
    if let Some(slots) = guard.get_mut(&(object as usize)) {
        slots.retain(|s| *s != slot as usize);
    }
    drop(guard);
    unsafe { *slot = std::ptr::null_mut() };
}

/// Zero every weak slot that pointed at `object`.
fn clear_weak_references(object: Id) {
    let Some(slots) = WEAK
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(object as usize))
    else {
        return;
    };
    for slot in slots {
        // SAFETY: a slot registered by `store_weak` and not yet destroyed. This
        // is the whole point of the registration: the owner is about to read a
        // pointer that would otherwise dangle.
        unsafe { *(slot as *mut Id) = std::ptr::null_mut() };
    }
}

/// One recursive lock per object, created on first use and never reclaimed.
static MONITORS: LazyLock<Mutex<HashMap<usize, &'static Monitor>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A recursive mutex, which is what `@synchronized` needs and what a plain
/// `Mutex` is not: the same thread may enter twice.
struct Monitor {
    owner: Mutex<Option<(u64, usize)>>,
    condition: std::sync::Condvar,
}

fn monitor(object: Id) -> &'static Monitor {
    let mut guard = MONITORS.lock().unwrap_or_else(|e| e.into_inner());
    guard.entry(object as usize).or_insert_with(|| {
        Box::leak(Box::new(Monitor {
            owner: Mutex::new(None),
            condition: std::sync::Condvar::new(),
        }))
    })
}

fn thread_id() -> u64 {
    // A value unique to this thread for as long as it runs, which is all the
    // recursion check needs.
    thread_local! {
        static SLOT: u8 = const { 0 };
    }
    SLOT.with(|slot| slot as *const u8 as u64)
}

pub fn sync_enter(object: Id) {
    if object.is_null() {
        return;
    }
    crate::debug::validate(object, "objc_sync_enter");
    let monitor = monitor(object);
    let me = thread_id();
    let mut owner = monitor.owner.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        match *owner {
            Some((holder, ref mut depth)) if holder == me => {
                *depth += 1;
                return;
            }
            None => {
                *owner = Some((me, 1));
                return;
            }
            Some(_) => {
                owner = monitor
                    .condition
                    .wait(owner)
                    .unwrap_or_else(|e| e.into_inner());
            }
        }
    }
}

pub fn sync_exit(object: Id) {
    if object.is_null() {
        return;
    }
    crate::debug::validate(object, "objc_sync_exit");
    let monitor = monitor(object);
    let mut owner = monitor.owner.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((holder, ref mut depth)) = *owner
        && holder == thread_id()
    {
        *depth -= 1;
        if *depth == 0 {
            *owner = None;
            monitor.condition.notify_one();
        }
        return;
    }
    if crate::debug::enabled() {
        crate::fail!(
            "objc debug: objc_sync_exit({object:p}) without a matching enter on this thread"
        );
    }
}

/// `objc_setAssociatedObject`'s table, keyed by object and key pointer.
///
/// Addresses rather than pointers, so the table is `Send` without an assertion
/// this crate would then have to justify.
static ASSOCIATIONS: LazyLock<RwLock<HashMap<(usize, usize), usize>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn set_associated(object: Id, key: *const c_void, value: Id) {
    crate::debug::validate(object, "objc_setAssociatedObject");
    crate::debug::validate(value, "objc_setAssociatedObject value");
    let mut map = ASSOCIATIONS.write().unwrap_or_else(|e| e.into_inner());
    match value.is_null() {
        true => map.remove(&(object as usize, key as usize)),
        false => map.insert((object as usize, key as usize), value as usize),
    };
}

pub fn associated(object: Id, key: *const c_void) -> Id {
    crate::debug::validate(object, "objc_getAssociatedObject");
    ASSOCIATIONS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(object as usize, key as usize))
        .map_or(std::ptr::null_mut(), |v| *v as Id)
}
