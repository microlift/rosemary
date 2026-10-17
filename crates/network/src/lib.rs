//! Network.framework's path monitor, after Apple's `<Network/path_monitor.h>`
//! and `<Network/path.h>`.
//!
//! A monitor, once given a queue and started, calls its update handler on that
//! queue with the current path, and again each time the path changes, until it
//! is cancelled; its cancel handler then runs once on the same queue. A path
//! is *satisfied* when some interface that is up, running and not loopback has
//! an address to send from, which is what this host's own interface list
//! answers. Nothing here notices a change the moment it happens: a started
//! monitor looks again every [`POLL`], so a change is reported within that.
//!
//! The objects are Objective-C objects, as Apple's are (`OS_nw_path_monitor`
//! and `OS_nw_path`): ARC retains and releases them, and `nw_retain` and
//! `nw_release` are the same two calls.

use std::ffi::c_void;
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
    status: Status,
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
        let class =
            corefoundation::base::register(c"OS_nw_path_monitor", None, size_of::<MonitorBody>(), false);
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
        corefoundation::base::register(c"OS_nw_path", None, size_of::<PathBody>(), false)
    })
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
fn make_path(status: Status) -> Id {
    // SAFETY: `PathBody` is the layout the class was registered with.
    let body: *mut PathBody = unsafe { corefoundation::base::allocate(path_class()) };
    if !body.is_null() {
        // SAFETY: freshly allocated.
        unsafe { (*body).status = status };
    }
    body as Id
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

/// The type Network.framework would give a host interface: loopback by its
/// flag, Wi-Fi where the host says the interface is wireless (Linux's
/// `/sys/class/net/<name>/wireless`), a point-to-point link as other, and the
/// rest wired. Nothing on a desktop host is cellular.
fn interface_type(name: &std::ffi::CStr, flags: libc::c_int) -> InterfaceType {
    if flags & libc::IFF_LOOPBACK != 0 {
        return InterfaceType::Loopback;
    }
    let sys = std::path::Path::new("/sys/class/net").join(name.to_string_lossy().as_ref());
    if sys.join("wireless").exists() || sys.join("phy80211").exists() {
        return InterfaceType::Wifi;
    }
    match flags & libc::IFF_POINTOPOINT != 0 {
        true => InterfaceType::Other,
        false => InterfaceType::Wired,
    }
}

/// What the host's interfaces say about reaching anything, through any type
/// but the prohibited ones (a mask over `1 << nw_interface_type_t`):
/// satisfied if one that is up and running, and not loopback, has an IPv4
/// address or an IPv6 one beyond link-local.
pub fn current_status(prohibited: u32) -> Status {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: a list pointer for `getifaddrs` to fill in, freed below.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return Status::Unsatisfied;
    }
    let mut usable = false;
    let mut at = list;
    while !at.is_null() {
        // SAFETY: a node of the list `getifaddrs` returned.
        let entry = unsafe { &*at };
        at = entry.ifa_next;
        let flags = entry.ifa_flags as libc::c_int;
        let up = flags & libc::IFF_UP != 0 && flags & libc::IFF_RUNNING != 0;
        if !up || flags & libc::IFF_LOOPBACK != 0 || entry.ifa_addr.is_null() {
            continue;
        }
        // SAFETY: `getifaddrs` names every interface it lists.
        let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) };
        if prohibited & 1 << interface_type(name, flags) as u32 != 0 {
            continue;
        }
        // SAFETY: a socket address of the family it states.
        usable |= match unsafe { (*entry.ifa_addr).sa_family } as libc::c_int {
            libc::AF_INET => true,
            libc::AF_INET6 => {
                let v6 = unsafe { &*(entry.ifa_addr as *const libc::sockaddr_in6) };
                let octets = v6.sin6_addr.s6_addr;
                !(octets[0] == 0xfe && octets[1] & 0xc0 == 0x80)
            }
            _ => false,
        };
    }
    // SAFETY: the list `getifaddrs` returned, freed once.
    unsafe { libc::freeifaddrs(list) };
    match usable {
        true => Status::Satisfied,
        false => Status::Unsatisfied,
    }
}

/// One handler call waiting on the monitor's queue.
struct Delivery {
    handler: Block,
    /// A path at +1, or nil for the cancel handler.
    path: Id,
}

unsafe extern "C" fn deliver(context: *mut c_void) {
    // SAFETY: a `Delivery` boxed by `post`, taken back once.
    let delivery = unsafe { Box::from_raw(context as *mut Delivery) };
    let invoke = unsafe { *((delivery.handler.id() as *const u8).add(16) as *const usize) };
    if invoke != 0 {
        // SAFETY: `nw_path_monitor_update_handler_t` is `void (^)(nw_path_t)`
        // and the cancel handler `void (^)(void)`; passing a nil second
        // argument to the latter is harmless under the C calling convention.
        unsafe {
            let f: unsafe extern "C" fn(Id, Id) = std::mem::transmute(invoke);
            f(delivery.handler.id(), delivery.path);
        }
    }
    objc::arc::release(delivery.path);
}

/// Run `handler` on `queue` with `path`, which this call owns.
fn post(queue: u64, handler: Block, path: Id) {
    let context = Box::into_raw(Box::new(Delivery { handler, path }));
    // SAFETY: a live queue the monitor retains, and `deliver` takes back the
    // box it is handed.
    unsafe { darwin::dispatch::queue::async_f(queue, context as u64, deliver as *const () as u64) };
}

/// Hand the update handler a path with `status`, unless the monitor has been
/// cancelled since.
fn report(monitor: &Monitor, status: Status) {
    let state = lock(monitor);
    if state.phase != Phase::Started || state.queue == 0 {
        return;
    }
    let Some(handler) = state.update.clone() else {
        return;
    };
    post(state.queue, handler, make_path(status));
}

/// What a started monitor's thread does: report the path, then again each
/// time it differs, until cancelled.
fn watch(monitor: Arc<Monitor>) {
    let mut last = None;
    loop {
        let prohibited = lock(&monitor).prohibited;
        let status = current_status(prohibited);
        if last != Some(status) {
            report(&monitor, status);
            last = Some(status);
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
fn cancel(monitor: &Monitor) {
    let mut state = lock(monitor);
    if state.phase == Phase::Cancelled {
        return;
    }
    state.phase = Phase::Cancelled;
    monitor.cancelled.notify_all();
    // No update after the cancel handler: the handler is dropped here, and a
    // report already queued runs before the cancel handler on the same queue.
    state.update = None;
    if let (Some(handler), queue) = (state.cancel.take(), state.queue)
        && queue != 0
    {
        post(queue, handler, std::ptr::null_mut());
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
        cancel(monitor);
    }
}

/// `void nw_path_monitor_cancel(nw_path_monitor_t)`
pub unsafe extern "C" fn nw_path_monitor_cancel(object: Id) {
    if let Some(monitor) = monitor_of(object) {
        cancel(monitor);
    }
}

/// `nw_path_status_t nw_path_get_status(nw_path_t)`
pub unsafe extern "C" fn nw_path_get_status(path: Id) -> i32 {
    if !is_instance(path, path_class()) {
        return Status::Invalid as i32;
    }
    // SAFETY: an instance of the path class.
    unsafe { (*(path as *const PathBody)).status as i32 }
}

/// `void *nw_retain(void *obj)`
pub unsafe extern "C" fn nw_retain(object: Id) -> Id {
    objc::arc::retain(object)
}

/// `void nw_release(void *obj)`
pub unsafe extern "C" fn nw_release(object: Id) {
    objc::arc::release(object);
}

unsafe extern "C" fn monitor_dealloc(receiver: Id, _sel: Sel) {
    let body = receiver as *mut MonitorBody;
    // SAFETY: `-dealloc` of a `MonitorBody` whose `monitor` came from
    // `Arc::into_raw`, taken back once.
    let monitor = unsafe { Arc::from_raw((*body).monitor) };
    let queue = {
        let mut state = lock(&monitor);
        // The last reference is gone, so nothing can cancel it any more; a
        // watcher still running stops here, and the handlers go with it.
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
        "nw_path_get_status" => nw_path_get_status,
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
            assert_eq!(STATUS.load(Ordering::SeqCst), current_status(0) as i32);

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
        assert_eq!(current_status(every), Status::Unsatisfied);
        assert_eq!(current_status(1 << InterfaceType::Loopback as u32), current_status(0));
    }

    #[test]
    fn something_that_is_not_a_path_has_no_status() {
        // SAFETY: nil and a non-path object are both refused.
        unsafe {
            assert_eq!(nw_path_get_status(std::ptr::null_mut()), Status::Invalid as i32);
            let monitor = nw_path_monitor_create();
            assert_eq!(nw_path_get_status(monitor), Status::Invalid as i32);
            nw_release(monitor);
        }
    }
}
