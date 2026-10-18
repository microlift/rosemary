//! Network.framework's path monitor, after Apple's `<Network/path_monitor.h>`
//! and `<Network/path.h>`.
//!
//! A monitor, once given a queue and started, calls its update handler on that
//! queue with the current path, and again each time the path changes, until it
//! is cancelled; its cancel handler then runs once on the same queue. A path
//! is *satisfied* when the host has a route out and some interface the path
//! may use is up with an address beyond its own link, which `starnix` answers
//! the same way for SCNetworkReachability. Nothing here notices a change the
//! moment it happens: a started
//! monitor looks again every [`POLL`], so a change is reported within that.
//!
//! The objects are Objective-C objects, as Apple's are (`OS_nw_path_monitor`,
//! `OS_nw_path` and `OS_nw_interface`): ARC retains and releases them, and `nw_retain` and
//! `nw_release` are the same two calls.

// The entry points are C functions whose safety is the C header's contract.
#![allow(clippy::missing_safety_doc)]

use std::ffi::{CString, c_char, c_void};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

/// How often a started monitor looks at the interfaces again.
const POLL: Duration = Duration::from_secs(2);

/// `nw_path_status_t`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Status {
    Invalid = 0,
    Satisfied = 1,
    Unsatisfied = 2,
    Satisfiable = 3,
}

/// The object an `nw_path_monitor_t` is.
#[repr(C)]
struct MonitorBody {
    isa: *const c_void,
    /// From `Arc::into_raw`, so the thread watching the interfaces can hold the
    /// state too.
    monitor: *const Monitor,
}

/// The object an `nw_path_t` is: a snapshot, which never changes once made.
#[repr(C)]
struct PathBody {
    isa: *const c_void,
    /// From `Box::into_raw`, freed by `-dealloc`.
    path: *mut Path,
}

/// What a path says, compared whole to tell whether it changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    pub status: Status,
    /// The interfaces the path may use, the one carrying the route first.
    pub interfaces: Vec<Interface>,
}

/// The object an `nw_interface_t` is, which owns a copy of what it names so
/// it outlives the path it came from.
#[repr(C)]
struct InterfaceBody {
    isa: *const c_void,
    /// From `Box::into_raw`, freed by `-dealloc`.
    data: *mut InterfaceData,
}

struct InterfaceData {
    interface: Interface,
    /// `name`, NUL-terminated for `nw_interface_get_name`.
    name: CString,
}

/// One interface a path may use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    pub index: u32,
    pub kind: InterfaceType,
}

struct Monitor {
    state: Mutex<State>,
    /// Signalled when the monitor is cancelled, so the watcher stops waiting.
    cancelled: Condvar,
}

#[derive(Default)]
struct State {
    /// A retained `dispatch_queue_t`, or zero.
    queue: u64,
    /// Copied blocks, or nil.
    update: Option<Block>,
    cancel: Option<Block>,
    phase: Phase,
    /// `1 << nw_interface_type_t` for each type the path may not use.
    prohibited: u32,
    /// The monitor object, retained from start until the cancel handler has
    /// run: Apple's keeps a started monitor alive after its creator lets go.
    held: usize,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Phase {
    #[default]
    Idle,
    Started,
    Cancelled,
}

/// A block this module copied, released when dropped.
struct Block(Id);

// SAFETY: a heap block is immutable once copied, and `_Block_copy` and
// `_Block_release` are thread-safe.
unsafe impl Send for Block {}

impl Block {
    fn copied(block: Id) -> Option<Block> {
        (!block.is_null()).then(|| Block(objc::block::copy(block)))
    }

    fn id(&self) -> Id {
        self.0
    }
}

impl Clone for Block {
    fn clone(&self) -> Block {
        Block(objc::block::copy(self.0))
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        objc::block::release(self.0);
    }
}

fn lock(monitor: &Monitor) -> MutexGuard<'_, State> {
    monitor.state.lock().unwrap_or_else(|e| e.into_inner())
}

fn monitor_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<&'static Class> = std::sync::OnceLock::new();
    CLASS.get_or_init(|| {
        let class = corefoundation::base::register(
            c"OS_nw_path_monitor",
            None,
            size_of::<MonitorBody>(),
            false,
        );
        class.add_method(
            objc::sel::get("dealloc"),
            monitor_dealloc as *const c_void,
            c"v16@0:8",
        );
        class
    })
}

fn path_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<&'static Class> = std::sync::OnceLock::new();
    CLASS.get_or_init(|| {
        let class =
            corefoundation::base::register(c"OS_nw_path", None, size_of::<PathBody>(), false);
        class.add_method(
            objc::sel::get("dealloc"),
            path_dealloc as *const c_void,
            c"v16@0:8",
        );
        class
    })
}

fn interface_class() -> &'static Class {
    static CLASS: std::sync::OnceLock<&'static Class> = std::sync::OnceLock::new();
    CLASS.get_or_init(|| {
        let class = corefoundation::base::register(
            c"OS_nw_interface",
            None,
            size_of::<InterfaceBody>(),
            false,
        );
        class.add_method(
            objc::sel::get("dealloc"),
            interface_dealloc as *const c_void,
            c"v16@0:8",
        );
        class
    })
}

/// A new interface at +1.
fn make_interface(interface: Interface) -> Id {
    // SAFETY: `InterfaceBody` is the layout the class was registered with.
    let body: *mut InterfaceBody = unsafe { corefoundation::base::allocate(interface_class()) };
    if !body.is_null() {
        // Interface names never hold a NUL, but a lossy name is better than none.
        let name = CString::new(interface.name.replace('\0', "")).unwrap_or_default();
        let data = Box::new(InterfaceData { interface, name });
        // SAFETY: freshly allocated.
        unsafe { (*body).data = Box::into_raw(data) };
    }
    body as Id
}

fn interface_of<'a>(object: Id) -> Option<&'a InterfaceData> {
    if !is_instance(object, interface_class()) {
        return None;
    }
    // SAFETY: an instance of the interface class, whose body `make_interface`
    // wrote.
    Some(unsafe { &*(*(object as *const InterfaceBody)).data })
}

/// Whether `object` is a non-nil instance of exactly `class`.
fn is_instance(object: Id, class: &'static Class) -> bool {
    // SAFETY: nil or an object, which is what the entry points are handed.
    !object.is_null()
        && unsafe { objc::object::class_of(object) }.is_some_and(|c| std::ptr::eq(c, class))
}

/// The state behind an `nw_path_monitor_t`, or `None` for anything else.
fn monitor_of<'a>(object: Id) -> Option<&'a Monitor> {
    if !is_instance(object, monitor_class()) {
        return None;
    }
    // SAFETY: an instance of the monitor class, whose body this module wrote.
    Some(unsafe { &*(*(object as *const MonitorBody)).monitor })
}

/// A new path at +1.
fn make_path(path: Path) -> Id {
    // SAFETY: `PathBody` is the layout the class was registered with.
    let body: *mut PathBody = unsafe { corefoundation::base::allocate(path_class()) };
    if !body.is_null() {
        // SAFETY: freshly allocated.
        unsafe { (*body).path = Box::into_raw(Box::new(path)) };
    }
    body as Id
}

/// What an `nw_path_t` says, or `None` for anything else.
fn path_of<'a>(object: Id) -> Option<&'a Path> {
    if !is_instance(object, path_class()) {
        return None;
    }
    // SAFETY: an instance of the path class, whose body `make_path` wrote.
    Some(unsafe { &*(*(object as *const PathBody)).path })
}

/// `nw_interface_type_t`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum InterfaceType {
    Other = 0,
    Wifi = 1,
    Cellular = 2,
    Wired = 3,
    Loopback = 4,
}

impl InterfaceType {
    fn of(link: starnix::net::Link) -> InterfaceType {
        use starnix::net::Link;
        match link {
            Link::Wifi => InterfaceType::Wifi,
            Link::Wired => InterfaceType::Wired,
            Link::Cellular => InterfaceType::Cellular,
            Link::Loopback => InterfaceType::Loopback,
            Link::Other => InterfaceType::Other,
        }
    }

    /// The order a path lists interfaces in when none carries the route.
    fn preference(self) -> u8 {
        match self {
            InterfaceType::Wired => 0,
            InterfaceType::Wifi => 1,
            InterfaceType::Cellular => 2,
            InterfaceType::Other => 3,
            InterfaceType::Loopback => 4,
        }
    }
}

/// The path the host's interfaces describe, through any type but the
/// prohibited ones (a mask over `1 << nw_interface_type_t`).
pub fn current_path(prohibited: u32) -> Path {
    let (v4, v6) = (starnix::net::route_v4(), starnix::net::route_v6());
    let mut usable: Vec<(bool, Interface)> = starnix::net::interfaces()
        .into_iter()
        .filter(starnix::net::Interface::routable)
        .map(|i| {
            let carries =
                v4.is_some_and(|a| i.v4.contains(&a)) || v6.is_some_and(|a| i.v6.contains(&a));
            let interface = Interface {
                kind: InterfaceType::of(i.link),
                name: i.name,
                index: i.index,
            };
            (carries, interface)
        })
        .filter(|(_, i)| prohibited & 1 << i.kind as u32 == 0)
        .collect();
    usable.sort_by_key(|(carries, i)| (!carries, i.kind.preference()));
    // FIXME (ACCURACY): only the default route is probed, so a path whose
    // route-carrying interface is prohibited counts as satisfied through any
    // other usable one, as a host with a route per interface would be.
    let routed = v4.is_some() || v6.is_some();
    Path {
        status: match routed && !usable.is_empty() {
            true => Status::Satisfied,
            false => Status::Unsatisfied,
        },
        interfaces: usable.into_iter().map(|(_, i)| i).collect(),
    }
}

/// One handler call waiting on the monitor's queue.
struct Delivery {
    handler: Block,
    /// A path at +1, or nil for the cancel handler.
    path: Id,
    /// An object to release once the handler has run, or nil.
    release: Id,
}

unsafe extern "C" fn deliver(context: *mut c_void) {
    // SAFETY: a `Delivery` boxed by `post`, taken back once.
    let delivery = unsafe { Box::from_raw(context as *mut Delivery) };
    // SAFETY: a block this module copied, alive until `delivery` drops.
    if let Some(invoke) = unsafe { objc::block::invoke(delivery.handler.id()) } {
        // SAFETY: `nw_path_monitor_update_handler_t` is `void (^)(nw_path_t)`
        // and the cancel handler `void (^)(void)`; passing a nil second
        // argument to the latter is harmless under the C calling convention.
        unsafe {
            let f: unsafe extern "C" fn(Id, Id) = std::mem::transmute(invoke);
            f(delivery.handler.id(), delivery.path);
        }
    }
    objc::arc::release(delivery.path);
    objc::arc::release(delivery.release);
}

/// Run `handler` on `queue` with `path`, then release `release`; this call
/// owns both.
fn post(queue: u64, handler: Block, path: Id, release: Id) {
    let context = Box::into_raw(Box::new(Delivery {
        handler,
        path,
        release,
    }));
    // SAFETY: a live queue the monitor retains, and `deliver` takes back the
    // box it is handed.
    unsafe { darwin::dispatch::queue::async_f(queue, context as u64, deliver as *const () as u64) };
}

/// Hand the update handler `path`, unless the monitor has been cancelled
/// since.
fn report(monitor: &Monitor, path: &Path) {
    let state = lock(monitor);
    if state.phase != Phase::Started || state.queue == 0 {
        return;
    }
    let Some(handler) = state.update.clone() else {
        return;
    };
    post(
        state.queue,
        handler,
        make_path(path.clone()),
        std::ptr::null_mut(),
    );
}

/// What a started monitor's thread does: report the path, then again each
/// time it differs, until cancelled.
fn watch(monitor: Arc<Monitor>) {
    let mut last = None;
    loop {
        let prohibited = lock(&monitor).prohibited;
        // Any change counts, not just one of status: Wi-Fi dropping while
        // wired stays up is a new path that is still satisfied.
        let path = current_path(prohibited);
        if last.as_ref() != Some(&path) {
            report(&monitor, &path);
            last = Some(path);
        }
        let state = lock(&monitor);
        if state.phase != Phase::Started {
            return;
        }
        let (state, _) = monitor
            .cancelled
            .wait_timeout_while(state, POLL, |s| s.phase == Phase::Started)
            .unwrap_or_else(|e| e.into_inner());
        if state.phase != Phase::Started {
            return;
        }
    }
}

/// Stop `monitor` and run its cancel handler, the first time only.
///
/// Answers the reference a started monitor holds on itself when nothing was
/// posted to release it; the caller releases it once `monitor` is no longer
/// borrowed, as that release may be the last.
#[must_use]
fn cancel(monitor: &Monitor) -> Id {
    let mut state = lock(monitor);
    if state.phase == Phase::Cancelled {
        return std::ptr::null_mut();
    }
    state.phase = Phase::Cancelled;
    monitor.cancelled.notify_all();
    // No update after the cancel handler: the handler is dropped here, and a
    // report already queued runs before the cancel handler on the same queue.
    state.update = None;
    let held = std::mem::take(&mut state.held) as Id;
    match (state.cancel.take(), state.queue) {
        (Some(handler), queue) if queue != 0 => {
            post(queue, handler, std::ptr::null_mut(), held);
            std::ptr::null_mut()
        }
        _ => held,
    }
}

/// `nw_path_monitor_t nw_path_monitor_create(void)`, at +1.
pub unsafe extern "C" fn nw_path_monitor_create() -> Id {
    // SAFETY: `MonitorBody` is the layout the class was registered with.
    let body: *mut MonitorBody = unsafe { corefoundation::base::allocate(monitor_class()) };
    if body.is_null() {
        return std::ptr::null_mut();
    }
    let monitor = Arc::new(Monitor {
        state: Mutex::new(State::default()),
        cancelled: Condvar::new(),
    });
    // SAFETY: freshly allocated.
    unsafe { (*body).monitor = Arc::into_raw(monitor) };
    body as Id
}

/// `nw_path_monitor_t nw_path_monitor_create_with_type(nw_interface_type_t)`,
/// at +1: a monitor whose paths use only interfaces of this type.
pub unsafe extern "C" fn nw_path_monitor_create_with_type(kind: i32) -> Id {
    // SAFETY: the entry point above.
    let object = unsafe { nw_path_monitor_create() };
    if let Some(monitor) = monitor_of(object) {
        lock(monitor).prohibited = match (0..32).contains(&kind) {
            true => !(1 << kind),
            false => u32::MAX,
        };
    }
    object
}

/// `void nw_path_monitor_set_queue(nw_path_monitor_t, dispatch_queue_t)`
pub unsafe extern "C" fn nw_path_monitor_set_queue(object: Id, queue: u64) {
    let Some(monitor) = monitor_of(object) else {
        return;
    };
    darwin::dispatch::object::retain(queue);
    let previous = std::mem::replace(&mut lock(monitor).queue, queue);
    // SAFETY: the reference this monitor took when it was set.
    unsafe { darwin::dispatch::object::release(previous) };
}

/// `void nw_path_monitor_set_update_handler(nw_path_monitor_t,
/// nw_path_monitor_update_handler_t)`
pub unsafe extern "C" fn nw_path_monitor_set_update_handler(object: Id, handler: Id) {
    if let Some(monitor) = monitor_of(object) {
        let mut state = lock(monitor);
        if state.phase != Phase::Cancelled {
            state.update = Block::copied(handler);
        }
    }
}

/// `void nw_path_monitor_set_cancel_handler(nw_path_monitor_t,
/// nw_path_monitor_cancel_handler_t)`
pub unsafe extern "C" fn nw_path_monitor_set_cancel_handler(object: Id, handler: Id) {
    if let Some(monitor) = monitor_of(object) {
        let mut state = lock(monitor);
        if state.phase != Phase::Cancelled {
            state.cancel = Block::copied(handler);
        }
    }
}

/// `void nw_path_monitor_prohibit_interface_type(nw_path_monitor_t,
/// nw_interface_type_t)`: the path the monitor reports may not use an
/// interface of this type.
pub unsafe extern "C" fn nw_path_monitor_prohibit_interface_type(object: Id, kind: i32) {
    if let Some(monitor) = monitor_of(object)
        && (0..32).contains(&kind)
    {
        lock(monitor).prohibited |= 1 << kind;
    }
}

/// `void nw_path_monitor_start(nw_path_monitor_t)`
///
/// A monitor starts once; starting one already started or cancelled does
/// nothing.
pub unsafe extern "C" fn nw_path_monitor_start(object: Id) {
    let Some(monitor) = monitor_of(object) else {
        return;
    };
    {
        let mut state = lock(monitor);
        if state.phase != Phase::Idle {
            return;
        }
        state.phase = Phase::Started;
        state.held = objc::arc::retain(object) as usize;
    }
    // SAFETY: `object` is a monitor, whose `Arc` the body holds a count of.
    let shared = unsafe {
        let raw = (*(object as *const MonitorBody)).monitor;
        Arc::increment_strong_count(raw);
        Arc::from_raw(raw)
    };
    let spawned = std::thread::Builder::new()
        .name("nw_path_monitor".into())
        .spawn(move || watch(shared));
    if spawned.is_err() {
        // Without a thread there is no one to report, and a monitor that will
        // never report is better told apart as cancelled.
        let held = cancel(monitor);
        objc::arc::release(held);
    }
}

/// `void nw_path_monitor_cancel(nw_path_monitor_t)`
pub unsafe extern "C" fn nw_path_monitor_cancel(object: Id) {
    let held = monitor_of(object).map_or(std::ptr::null_mut(), cancel);
    objc::arc::release(held);
}

/// `nw_path_status_t nw_path_get_status(nw_path_t)`
pub unsafe extern "C" fn nw_path_get_status(path: Id) -> i32 {
    path_of(path).map_or(Status::Invalid, |p| p.status) as i32
}

/// `bool nw_path_is_equal(nw_path_t, nw_path_t)`
pub unsafe extern "C" fn nw_path_is_equal(path: Id, other: Id) -> bool {
    path_of(path).is_some_and(|p| path_of(other) == Some(p))
}

/// `bool nw_path_uses_interface_type(nw_path_t, nw_interface_type_t)`
pub unsafe extern "C" fn nw_path_uses_interface_type(path: Id, kind: i32) -> bool {
    path_of(path).is_some_and(|p| p.interfaces.iter().any(|i| i.kind as i32 == kind))
}

/// `bool nw_path_is_expensive(nw_path_t)`: Apple's means cellular, or a
/// hotspot it knows to be one, and only the first is visible from a host.
pub unsafe extern "C" fn nw_path_is_expensive(path: Id) -> bool {
    path_of(path)
        .and_then(|p| p.interfaces.first())
        .is_some_and(|i| i.kind == InterfaceType::Cellular)
}

/// `bool nw_path_is_constrained(nw_path_t)`: Low Data Mode, which no host
/// here has a setting for.
pub unsafe extern "C" fn nw_path_is_constrained(_path: Id) -> bool {
    false
}

/// `void nw_path_enumerate_interfaces(nw_path_t,
/// nw_path_enumerate_interfaces_block_t)`: the block is `bool (^)(nw_interface_t)`
/// and runs before this returns, once per interface until it answers false.
pub unsafe extern "C" fn nw_path_enumerate_interfaces(path: Id, block: Id) {
    let Some(path) = path_of(path) else {
        return;
    };
    // SAFETY: nil or the caller's block.
    let Some(invoke) = (unsafe { objc::block::invoke(block) }) else {
        return;
    };
    // SAFETY: the enumerate block's prototype.
    let f: unsafe extern "C" fn(Id, Id) -> bool = unsafe { std::mem::transmute(invoke) };
    for interface in &path.interfaces {
        let object = make_interface(interface.clone());
        // SAFETY: the caller's block, handed an interface it may retain.
        let more = unsafe { f(block, object) };
        objc::arc::release(object);
        if !more {
            break;
        }
    }
}

/// `nw_interface_type_t nw_interface_get_type(nw_interface_t)`
pub unsafe extern "C" fn nw_interface_get_type(interface: Id) -> i32 {
    interface_of(interface).map_or(InterfaceType::Other, |i| i.interface.kind) as i32
}

/// `const char *nw_interface_get_name(nw_interface_t)`, alive as long as the
/// interface is.
pub unsafe extern "C" fn nw_interface_get_name(interface: Id) -> *const c_char {
    interface_of(interface).map_or(std::ptr::null(), |i| i.name.as_ptr())
}

/// `uint32_t nw_interface_get_index(nw_interface_t)`
pub unsafe extern "C" fn nw_interface_get_index(interface: Id) -> u32 {
    interface_of(interface).map_or(0, |i| i.interface.index)
}

/// `void *nw_retain(void *obj)`
pub unsafe extern "C" fn nw_retain(object: Id) -> Id {
    objc::arc::retain(object)
}

/// `void nw_release(void *obj)`
pub unsafe extern "C" fn nw_release(object: Id) {
    objc::arc::release(object);
}

unsafe extern "C" fn path_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: `-dealloc` of a `PathBody` whose `path` came from
    // `Box::into_raw`, taken back once.
    drop(unsafe { Box::from_raw((*(receiver as *mut PathBody)).path) });
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn interface_dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: `-dealloc` of an `InterfaceBody` whose `data` came from
    // `Box::into_raw`, taken back once.
    drop(unsafe { Box::from_raw((*(receiver as *mut InterfaceBody)).data) });
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn monitor_dealloc(receiver: Id, _sel: Sel) {
    let body = receiver as *mut MonitorBody;
    // SAFETY: `-dealloc` of a `MonitorBody` whose `monitor` came from
    // `Arc::into_raw`, taken back once.
    let monitor = unsafe { Arc::from_raw((*body).monitor) };
    let queue = {
        let mut state = lock(&monitor);
        // A started monitor holds itself until cancelled, so this is one that
        // never started or has already run its cancel handler.
        state.phase = Phase::Cancelled;
        monitor.cancelled.notify_all();
        state.update = None;
        state.cancel = None;
        std::mem::take(&mut state.queue)
    };
    // SAFETY: the reference `nw_path_monitor_set_queue` took.
    unsafe { darwin::dispatch::object::release(queue) };
    objc::arc::finalize(receiver);
}

exports::exports! {
    functions {
        "nw_path_monitor_create" => nw_path_monitor_create,
        "nw_path_monitor_set_queue" => nw_path_monitor_set_queue,
        "nw_path_monitor_set_update_handler" => nw_path_monitor_set_update_handler,
        "nw_path_monitor_set_cancel_handler" => nw_path_monitor_set_cancel_handler,
        "nw_path_monitor_prohibit_interface_type" => nw_path_monitor_prohibit_interface_type,
        "nw_path_monitor_start" => nw_path_monitor_start,
        "nw_path_monitor_cancel" => nw_path_monitor_cancel,
        "nw_path_monitor_create_with_type" => nw_path_monitor_create_with_type,
        "nw_path_get_status" => nw_path_get_status,
        "nw_path_is_equal" => nw_path_is_equal,
        "nw_path_uses_interface_type" => nw_path_uses_interface_type,
        "nw_path_is_expensive" => nw_path_is_expensive,
        "nw_path_is_constrained" => nw_path_is_constrained,
        "nw_path_enumerate_interfaces" => nw_path_enumerate_interfaces,
        "nw_interface_get_type" => nw_interface_get_type,
        "nw_interface_get_name" => nw_interface_get_name,
        "nw_interface_get_index" => nw_interface_get_index,
        "nw_retain" => nw_retain,
        "nw_release" => nw_release,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

    #[repr(C)]
    struct GlobalBlock {
        isa: *const c_void,
        flags: i32,
        reserved: i32,
        invoke: *const c_void,
        descriptor: *const [u64; 2],
    }

    static DESCRIPTOR: [u64; 2] = [0, size_of::<GlobalBlock>() as u64];

    /// A global block, which `_Block_copy` hands back unchanged, so a test
    /// takes the copy path without descriptor helpers having to exist.
    fn block(invoke: *const c_void) -> Box<GlobalBlock> {
        objc::bootstrap();
        let isa = objc::class::lookup(c"__NSGlobalBlock__")
            .map(|class| objc::class::ptr(class) as *const c_void)
            .expect("registered by `objc::block`");
        Box::new(GlobalBlock {
            isa,
            flags: 1 << 28,
            reserved: 0,
            invoke,
            descriptor: &DESCRIPTOR,
        })
    }

    static STATUS: AtomicI32 = AtomicI32::new(-1);
    static UPDATES: AtomicUsize = AtomicUsize::new(0);
    static CANCELS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn on_update(_block: Id, path: Id) {
        // SAFETY: the path the monitor hands over.
        STATUS.store(unsafe { nw_path_get_status(path) }, Ordering::SeqCst);
        UPDATES.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn on_cancel(_block: Id) {
        CANCELS.fetch_add(1, Ordering::SeqCst);
    }

    fn wait_for(done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Started, the monitor reports the path the interfaces describe on its
    /// queue; cancelled, it runs the cancel handler once and reports nothing
    /// more.
    #[test]
    fn a_started_monitor_reports_and_a_cancelled_one_stops() {
        let update = block(on_update as *const c_void);
        let cancelled = block(on_cancel as *const c_void);
        let queue = darwin::dispatch::queue::create_serial("nw test");
        // SAFETY: the entry points, called as a guest would.
        unsafe {
            let monitor = nw_path_monitor_create();
            nw_path_monitor_set_queue(monitor, queue);
            nw_path_monitor_set_update_handler(monitor, update.as_ref() as *const _ as Id);
            nw_path_monitor_set_cancel_handler(monitor, cancelled.as_ref() as *const _ as Id);
            nw_path_monitor_start(monitor);
            wait_for(|| UPDATES.load(Ordering::SeqCst) == 1);
            assert_eq!(STATUS.load(Ordering::SeqCst), current_path(0).status as i32);

            nw_path_monitor_cancel(monitor);
            nw_path_monitor_cancel(monitor);
            wait_for(|| CANCELS.load(Ordering::SeqCst) == 1);
            std::thread::sleep(POLL + Duration::from_millis(200));
            assert_eq!(CANCELS.load(Ordering::SeqCst), 1);
            assert_eq!(UPDATES.load(Ordering::SeqCst), 1);
            nw_release(monitor);
            darwin::dispatch::object::release(queue);
        }
    }

    static HELD_UPDATES: AtomicUsize = AtomicUsize::new(0);
    static HELD_CANCELS: AtomicUsize = AtomicUsize::new(0);
    static HELD_MONITOR: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn on_held_update(_block: Id, _path: Id) {
        if HELD_UPDATES.fetch_add(1, Ordering::SeqCst) == 0 {
            // SAFETY: the monitor the test started, still alive because it
            // is started.
            unsafe { nw_path_monitor_cancel(HELD_MONITOR.load(Ordering::SeqCst) as Id) };
        }
    }

    unsafe extern "C" fn on_held_cancel(_block: Id) {
        HELD_CANCELS.fetch_add(1, Ordering::SeqCst);
    }

    /// The usual C pattern: start, then let go at once. The monitor keeps
    /// reporting until it is cancelled, and its cancel handler still runs.
    #[test]
    fn a_started_monitor_outlives_its_creators_reference() {
        let update = block(on_held_update as *const c_void);
        let cancelled = block(on_held_cancel as *const c_void);
        let queue = darwin::dispatch::queue::create_serial("nw held test");
        // SAFETY: the entry points, called as a guest would.
        unsafe {
            let monitor = nw_path_monitor_create();
            HELD_MONITOR.store(monitor as usize, Ordering::SeqCst);
            nw_path_monitor_set_queue(monitor, queue);
            nw_path_monitor_set_update_handler(monitor, update.as_ref() as *const _ as Id);
            nw_path_monitor_set_cancel_handler(monitor, cancelled.as_ref() as *const _ as Id);
            nw_path_monitor_start(monitor);
            nw_release(monitor);
            wait_for(|| HELD_CANCELS.load(Ordering::SeqCst) == 1);
            assert_eq!(HELD_UPDATES.load(Ordering::SeqCst), 1);
            darwin::dispatch::object::release(queue);
        }
    }

    /// With every type an interface can have prohibited, nothing is left to
    /// reach anything through.
    #[test]
    fn prohibiting_every_interface_type_leaves_the_path_unsatisfied() {
        let every = [
            InterfaceType::Other,
            InterfaceType::Wifi,
            InterfaceType::Cellular,
            InterfaceType::Wired,
            InterfaceType::Loopback,
        ]
        .iter()
        .fold(0, |mask, t| mask | 1 << *t as u32);
        let none = current_path(every);
        assert_eq!(none.status, Status::Unsatisfied);
        assert!(none.interfaces.is_empty());
        assert_eq!(
            current_path(1 << InterfaceType::Loopback as u32),
            current_path(0)
        );
    }

    static SEEN: AtomicUsize = AtomicUsize::new(0);
    static FIRST_KIND: AtomicI32 = AtomicI32::new(-1);
    static FIRST_INDEX: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn on_interface(_block: Id, interface: Id) -> bool {
        // SAFETY: the interface the path hands over.
        unsafe {
            FIRST_KIND.store(nw_interface_get_type(interface), Ordering::SeqCst);
            FIRST_INDEX.store(nw_interface_get_index(interface) as usize, Ordering::SeqCst);
            let name = std::ffi::CStr::from_ptr(nw_interface_get_name(interface));
            assert_eq!(name.to_bytes(), b"wwan0");
        }
        SEEN.fetch_add(1, Ordering::SeqCst);
        false
    }

    /// A path answers what it uses from its interface list, in order, and
    /// enumeration stops when the block says so.
    #[test]
    fn a_path_answers_for_its_interfaces() {
        let path = make_path(Path {
            status: Status::Satisfied,
            interfaces: vec![
                Interface {
                    name: "wwan0".into(),
                    index: 7,
                    kind: InterfaceType::Cellular,
                },
                Interface {
                    name: "wlan0".into(),
                    index: 3,
                    kind: InterfaceType::Wifi,
                },
            ],
        });
        let each = block(on_interface as *const c_void);
        // SAFETY: the entry points, called as a guest would.
        unsafe {
            assert!(nw_path_uses_interface_type(
                path,
                InterfaceType::Wifi as i32
            ));
            assert!(!nw_path_uses_interface_type(
                path,
                InterfaceType::Wired as i32
            ));
            assert!(nw_path_is_expensive(path));
            assert!(!nw_path_is_constrained(path));
            assert!(nw_path_is_equal(path, path));
            nw_path_enumerate_interfaces(path, each.as_ref() as *const _ as Id);
            assert_eq!(SEEN.load(Ordering::SeqCst), 1);
            assert_eq!(
                FIRST_KIND.load(Ordering::SeqCst),
                InterfaceType::Cellular as i32
            );
            assert_eq!(FIRST_INDEX.load(Ordering::SeqCst), 7);
            nw_release(path);
        }
    }

    /// A monitor made for one type reports paths through that type only.
    #[test]
    fn a_typed_monitor_prohibits_every_other_type() {
        // SAFETY: the entry points, called as a guest would.
        unsafe {
            let monitor = nw_path_monitor_create_with_type(InterfaceType::Wifi as i32);
            let prohibited = lock(monitor_of(monitor).unwrap()).prohibited;
            assert_eq!(prohibited & 1 << InterfaceType::Wifi as u32, 0);
            assert_ne!(prohibited & 1 << InterfaceType::Wired as u32, 0);
            assert!(
                current_path(prohibited)
                    .interfaces
                    .iter()
                    .all(|i| i.kind == InterfaceType::Wifi)
            );
            nw_release(monitor);
        }
    }

    #[test]
    fn something_that_is_not_a_path_has_no_status() {
        // SAFETY: nil and a non-path object are both refused.
        unsafe {
            assert_eq!(
                nw_path_get_status(std::ptr::null_mut()),
                Status::Invalid as i32
            );
            let monitor = nw_path_monitor_create();
            assert_eq!(nw_path_get_status(monitor), Status::Invalid as i32);
            nw_release(monitor);
        }
    }
}
