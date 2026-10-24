//! `dispatch_source_t`, on this crate's own `kqueue`.
//!
//! One monitor thread waits on one kqueue for every armed source and hands
//! each event to the source's target queue. The kqueue's `udata` is an id
//! rather than the source's address: a cancelled source may still have an
//! event in flight, and an id that is no longer registered is dropped where a
//! freed address would be read.

use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use super::block;
use super::object::{self, Function, Header, Kind};
use super::queue;
use crate::kqueue::Kevent;

const EVFILT_READ: i16 = -1;
const EVFILT_WRITE: i16 = -2;
const EVFILT_SIGNAL: i16 = -6;
const EVFILT_USER: i16 = -10;

const EV_ADD: u16 = 0x0001;
const EV_DELETE: u16 = 0x0002;
const EV_ENABLE: u16 = 0x0004;
const EV_CLEAR: u16 = 0x0020;
const EV_DISPATCH: u16 = 0x0080;

const NOTE_TRIGGER: u32 = 0x0100_0000;

/// The `_dispatch_source_type_*` a guest passes, which are compared by address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
enum Type {
    DataAdd,
    DataOr,
    DataReplace,
    Read,
    Write,
    Signal,
    Vnode,
}

const TYPES: [Type; 7] = [
    Type::DataAdd,
    Type::DataOr,
    Type::DataReplace,
    Type::Read,
    Type::Write,
    Type::Signal,
    Type::Vnode,
];

/// One word per type; only the addresses matter.
static TYPE_OBJECTS: [u64; 7] = [0; 7];

fn type_object(t: Type) -> u64 {
    &TYPE_OBJECTS[t as usize] as *const u64 as u64
}

fn type_of(address: u64) -> Option<Type> {
    TYPES.into_iter().find(|t| type_object(*t) == address)
}

pub fn type_data_add() -> u64 {
    type_object(Type::DataAdd)
}
pub fn type_data_or() -> u64 {
    type_object(Type::DataOr)
}
pub fn type_data_replace() -> u64 {
    type_object(Type::DataReplace)
}
pub fn type_read() -> u64 {
    type_object(Type::Read)
}
pub fn type_write() -> u64 {
    type_object(Type::Write)
}
pub fn type_signal() -> u64 {
    type_object(Type::Signal)
}
pub fn type_vnode() -> u64 {
    type_object(Type::Vnode)
}

enum Handler {
    /// A copy this source owns.
    Block(u64),
    /// Called with the source's context, as the `_f` setters promise.
    Function(Function),
}

impl Handler {
    /// # Safety
    ///
    /// Guest code whose pointers are thunks already.
    unsafe fn run(&self, context: u64) {
        match self {
            Handler::Block(b) => unsafe { block::invoke(*b) },
            Handler::Function(f) => unsafe { f(context as *mut c_void) },
        }
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        if let Handler::Block(b) = self {
            block::release(*b);
        }
    }
}

#[derive(Default)]
struct State {
    event: Option<std::sync::Arc<Handler>>,
    cancel: Option<Handler>,
    /// Starts at one: a source does nothing until its first resume.
    suspended: u32,
    /// Merged but not yet delivered.
    pending: u64,
    /// The monitor's id for this source, once armed.
    id: Option<u64>,
    /// A delivery is queued or running. A source's handler never runs
    /// alongside itself, even on a concurrent queue.
    scheduled: bool,
    cancelled: bool,
    cancel_queued: bool,
}

/// `#[repr(C)]` with the header first, for the same reason as
/// [`super::queue::Queue`].
#[repr(C)]
pub struct Source {
    #[allow(dead_code)]
    header: Header,
    kind: Type,
    handle: u64,
    mask: u64,
    queue: u64,
    /// What `dispatch_source_get_data` answers inside the handler.
    data: AtomicU64,
    state: Mutex<State>,
}

impl Drop for Source {
    fn drop(&mut self) {
        // SAFETY: the reference `create` took.
        unsafe { object::release(self.queue) };
    }
}

/// # Safety
///
/// `handle` is null or an object this module's crate allocated.
unsafe fn source<'a>(handle: u64) -> Option<&'a Source> {
    // SAFETY: the kind check makes the cast sound.
    unsafe { object::typed(handle, Kind::Source) }
        .map(|h| unsafe { &*(h as *const Header as *const Source) })
}

impl Source {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn self_handle(&self) -> u64 {
        self as *const Source as u64
    }

    fn merge(&self, value: u64) {
        let mut state = self.lock();
        state.pending = match self.kind {
            Type::DataAdd | Type::Signal => state.pending.wrapping_add(value),
            Type::DataOr | Type::Vnode => state.pending | value,
            // Readiness reports the current amount, not a sum.
            Type::DataReplace | Type::Read | Type::Write => value,
        };
        self.schedule(&mut state);
    }

    /// Queue a delivery if one is owed and none is in flight.
    fn schedule(&self, state: &mut State) {
        if state.scheduled || state.suspended > 0 || state.cancelled || state.pending == 0 {
            return;
        }
        state.scheduled = true;
        let handle = self.self_handle();
        object::retain(handle);
        // SAFETY: `deliver` is a `dispatch_function_t` taking the handle,
        // which holds the reference just taken.
        unsafe { queue::async_f(self.queue, handle, deliver as *const () as u64) };
    }

    /// Queue the cancel handler, once, when nothing else is in flight.
    fn finish_cancel(&self, state: &mut State) {
        if !state.cancelled || state.scheduled || state.cancel_queued || state.suspended > 0 {
            return;
        }
        state.cancel_queued = true;
        state.event = None;
        let Some(handler) = state.cancel.take() else {
            return;
        };
        let work = Box::into_raw(Box::new((handler, self.context())));
        // SAFETY: `run_cancel` takes ownership of the box.
        unsafe { queue::async_f(self.queue, work as u64, run_cancel as *const () as u64) };
    }

    fn context(&self) -> u64 {
        // SAFETY: a live object.
        unsafe { object::get_context(self.self_handle()) as u64 }
    }
}

unsafe extern "C" fn run_cancel(work: *mut c_void) {
    // SAFETY: boxed by `finish_cancel` and run once.
    let work = unsafe { Box::from_raw(work as *mut (Handler, u64)) };
    unsafe { work.0.run(work.1) };
}

/// Run one delivery on the target queue.
unsafe extern "C" fn deliver(context: *mut c_void) {
    let handle = context as u64;
    // SAFETY: `schedule` took a reference for this call.
    let Some(s) = (unsafe { source(handle) }) else {
        return;
    };
    let handler = {
        let mut state = s.lock();
        let data = std::mem::take(&mut state.pending);
        s.data.store(data, Ordering::Release);
        if state.cancelled || data == 0 {
            None
        } else {
            state.event.clone()
        }
    };
    if let Some(handler) = handler {
        // SAFETY: guest code, as the setter received it.
        unsafe { handler.run(s.context()) };
    }
    {
        let mut state = s.lock();
        state.scheduled = false;
        if state.cancelled {
            s.finish_cancel(&mut state);
        } else {
            // Level-triggered readiness was disabled when it fired, so the
            // handler had the chance to consume it first.
            if matches!(s.kind, Type::Read | Type::Write)
                && let Some(id) = state.id
            {
                monitor().change(s, id, EV_ENABLE);
            }
            s.schedule(&mut state);
        }
    }
    // SAFETY: the reference `schedule` took.
    unsafe { object::release(handle) };
}

struct Monitor {
    kq: c_int,
    /// Armed sources by id, each holding a reference.
    sources: Mutex<HashMap<u64, u64>>,
    next: AtomicU64,
}

/// The wake-up `EVFILT_USER` ident; ids start above it.
const WAKE: u64 = 0;

fn monitor() -> &'static Monitor {
    static MONITOR: OnceLock<Monitor> = OnceLock::new();
    MONITOR.get_or_init(|| {
        let kq = crate::kqueue::kqueue();
        let monitor = Monitor {
            kq,
            sources: Mutex::new(HashMap::new()),
            next: AtomicU64::new(WAKE + 1),
        };
        monitor.submit(&Kevent {
            ident: WAKE as usize,
            filter: EVFILT_USER,
            flags: EV_ADD | EV_CLEAR,
            ..Kevent::default()
        });
        let _ = std::thread::Builder::new()
            .name("dispatch-sources".into())
            .spawn(move || run_monitor(kq));
        monitor
    })
}

impl Monitor {
    fn submit(&self, change: &Kevent) -> bool {
        // SAFETY: one change, no events, from this crate's own kqueue.
        let r = unsafe {
            crate::kqueue::kevent(
                self.kq,
                change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        r == 0
    }

    /// End the monitor's current wait, so it waits again on what changed.
    fn wake(&self) {
        self.submit(&Kevent {
            ident: WAKE as usize,
            filter: EVFILT_USER,
            fflags: NOTE_TRIGGER,
            ..Kevent::default()
        });
    }

    fn filter(kind: Type) -> Option<i16> {
        match kind {
            Type::Read => Some(EVFILT_READ),
            Type::Write => Some(EVFILT_WRITE),
            Type::Signal => Some(EVFILT_SIGNAL),
            _ => None,
        }
    }

    fn change(&self, s: &Source, id: u64, flags: u16) {
        let Some(filter) = Self::filter(s.kind) else {
            return;
        };
        self.submit(&Kevent {
            ident: s.handle as usize,
            filter,
            flags,
            udata: id as usize,
            ..Kevent::default()
        });
        self.wake();
    }

    /// Start watching `s`, answering with its id.
    fn arm(&self, s: &Source) -> Option<u64> {
        let filter = Self::filter(s.kind)?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let handle = s.self_handle();
        object::retain(handle);
        self.sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, handle);
        let flags = match filter {
            EVFILT_SIGNAL => EV_ADD,
            _ => EV_ADD | EV_DISPATCH,
        };
        let added = self.submit(&Kevent {
            ident: s.handle as usize,
            filter,
            flags,
            udata: id as usize,
            ..Kevent::default()
        });
        if !added {
            self.disarm(s, id);
            return None;
        }
        self.wake();
        Some(id)
    }

    fn disarm(&self, s: &Source, id: u64) {
        if let Some(filter) = Self::filter(s.kind) {
            self.submit(&Kevent {
                ident: s.handle as usize,
                filter,
                flags: EV_DELETE,
                ..Kevent::default()
            });
        }
        let handle = self
            .sources
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        if let Some(handle) = handle {
            // SAFETY: the reference `arm` took.
            unsafe { object::release(handle) };
        }
    }
}

fn run_monitor(kq: c_int) {
    let mut events = [Kevent::default(); 16];
    loop {
        // SAFETY: room for `events.len()` events and no timeout.
        let n = unsafe {
            crate::kqueue::kevent(
                kq,
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                events.len() as c_int,
                std::ptr::null(),
            )
        };
        if n < 0 {
            if crate::errno::get() == libc::EINTR {
                continue;
            }
            return;
        }
        for event in &events[..n as usize] {
            if event.filter == EVFILT_USER {
                continue;
            }
            let id = event.udata as u64;
            let monitor = monitor();
            let handle = {
                let sources = monitor.sources.lock().unwrap_or_else(|e| e.into_inner());
                let handle = sources.get(&id).copied();
                if let Some(handle) = handle {
                    object::retain(handle);
                }
                handle
            };
            let Some(handle) = handle else {
                continue;
            };
            // SAFETY: the reference just taken.
            if let Some(s) = unsafe { source(handle) } {
                // Zero readable bytes at end of file is still an event.
                let value = match s.kind {
                    Type::Signal => event.data.max(1) as u64,
                    _ => (event.data as u64).max(1),
                };
                s.merge(value);
            }
            // SAFETY: as above.
            unsafe { object::release(handle) };
        }
    }
}

/// `dispatch_source_t dispatch_source_create(dispatch_source_type_t,
/// uintptr_t handle, uintptr_t mask, dispatch_queue_t)`.
pub unsafe extern "C" fn create(kind: u64, handle: u64, mask: u64, target: u64) -> u64 {
    let Some(kind) = type_of(kind) else {
        return 0;
    };
    if kind == Type::Vnode {
        static ONCE: std::sync::Once = std::sync::Once::new();
        // FIXME (ACCURACY): the kqueue has no EVFILT_VNODE, so a vnode source
        // is accepted but never fires.
        ONCE.call_once(|| eprintln!("rosemary: dispatch: vnode sources never fire"));
    }
    let target = match target {
        // SAFETY: no flags, a valid identifier.
        0 => unsafe { queue::get_global_queue(0, 0) },
        q => q,
    };
    object::retain(target);
    let source = Box::new(Source {
        header: Header::new(Kind::Source),
        kind,
        handle,
        mask,
        queue: target,
        data: AtomicU64::new(0),
        state: Mutex::new(State {
            suspended: 1,
            ..State::default()
        }),
    });
    Box::into_raw(source) as u64
}

fn set_handler(handle: u64, handler: Option<Handler>, cancel: bool) {
    // SAFETY: the guest holds a reference for the call.
    let Some(s) = (unsafe { source(handle) }) else {
        return;
    };
    let mut state = s.lock();
    if cancel {
        state.cancel = handler;
    } else {
        state.event = handler.map(std::sync::Arc::new);
    }
}

fn block_handler(block: u64) -> Option<Handler> {
    block::copy(block).map(Handler::Block)
}

fn function_handler(function: u64) -> Option<Handler> {
    // SAFETY: a `dispatch_function_t`.
    (function != 0)
        .then(|| Handler::Function(unsafe { std::mem::transmute::<u64, Function>(function) }))
}

/// `void dispatch_source_set_event_handler(dispatch_source_t, dispatch_block_t)`.
pub unsafe extern "C" fn set_event_handler(handle: u64, block: u64) {
    set_handler(handle, block_handler(block), false);
}

/// `void dispatch_source_set_event_handler_f(dispatch_source_t, dispatch_function_t)`.
pub unsafe extern "C" fn set_event_handler_f(handle: u64, function: u64) {
    set_handler(handle, function_handler(function), false);
}

/// `void dispatch_source_set_cancel_handler(dispatch_source_t, dispatch_block_t)`.
pub unsafe extern "C" fn set_cancel_handler(handle: u64, block: u64) {
    set_handler(handle, block_handler(block), true);
}

/// `void dispatch_source_set_cancel_handler_f(dispatch_source_t, dispatch_function_t)`.
pub unsafe extern "C" fn set_cancel_handler_f(handle: u64, function: u64) {
    set_handler(handle, function_handler(function), true);
}

/// `void dispatch_source_cancel(dispatch_source_t)`.
pub unsafe extern "C" fn cancel(handle: u64) {
    // SAFETY: the guest holds a reference for the call.
    let Some(s) = (unsafe { source(handle) }) else {
        return;
    };
    let id = {
        let mut state = s.lock();
        if state.cancelled {
            return;
        }
        state.cancelled = true;
        state.id.take()
    };
    if let Some(id) = id {
        monitor().disarm(s, id);
    }
    let mut state = s.lock();
    s.finish_cancel(&mut state);
}

/// `long dispatch_source_testcancel(dispatch_source_t)`.
pub unsafe extern "C" fn testcancel(handle: u64) -> i64 {
    // SAFETY: the guest holds a reference for the call.
    unsafe { source(handle) }.map_or(0, |s| s.lock().cancelled as i64)
}

/// `unsigned long dispatch_source_get_data(dispatch_source_t)`.
pub unsafe extern "C" fn get_data(handle: u64) -> u64 {
    // SAFETY: the guest holds a reference for the call.
    unsafe { source(handle) }.map_or(0, |s| s.data.load(Ordering::Acquire))
}

/// `uintptr_t dispatch_source_get_handle(dispatch_source_t)`.
pub unsafe extern "C" fn get_handle(handle: u64) -> u64 {
    // SAFETY: the guest holds a reference for the call.
    unsafe { source(handle) }.map_or(0, |s| s.handle)
}

/// `unsigned long dispatch_source_get_mask(dispatch_source_t)`.
pub unsafe extern "C" fn get_mask(handle: u64) -> u64 {
    // SAFETY: the guest holds a reference for the call.
    unsafe { source(handle) }.map_or(0, |s| s.mask)
}

/// `void dispatch_source_merge_data(dispatch_source_t, unsigned long)`.
pub unsafe extern "C" fn merge_data(handle: u64, value: u64) {
    // SAFETY: the guest holds a reference for the call.
    let Some(s) = (unsafe { source(handle) }) else {
        return;
    };
    if matches!(s.kind, Type::DataAdd | Type::DataOr | Type::DataReplace) && value != 0 {
        s.merge(value);
    }
}

/// `dispatch_resume` for a source: the first one arms it.
pub(crate) fn resume(handle: u64) -> bool {
    // SAFETY: the guest holds a reference for the call.
    let Some(s) = (unsafe { source(handle) }) else {
        return false;
    };
    let mut state = s.lock();
    if state.suspended == 0 {
        return true;
    }
    state.suspended -= 1;
    if state.suspended > 0 {
        return true;
    }
    if state.id.is_none() && !state.cancelled {
        drop(state);
        let id = monitor().arm(s);
        state = s.lock();
        if state.cancelled {
            drop(state);
            if let Some(id) = id {
                monitor().disarm(s, id);
            }
            return true;
        }
        state.id = id;
    }
    s.schedule(&mut state);
    s.finish_cancel(&mut state);
    true
}

/// `dispatch_suspend` for a source.
pub(crate) fn suspend(handle: u64) -> bool {
    // SAFETY: the guest holds a reference for the call.
    let Some(s) = (unsafe { source(handle) }) else {
        return false;
    };
    s.lock().suspended += 1;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    static FIRED: AtomicU32 = AtomicU32::new(0);
    static SEEN: AtomicU64 = AtomicU64::new(0);
    static CANCELLED: AtomicU32 = AtomicU32::new(0);

    unsafe extern "C" fn on_event(context: *mut c_void) {
        let source = context as u64;
        SEEN.fetch_add(unsafe { get_data(source) }, Ordering::AcqRel);
        FIRED.fetch_add(1, Ordering::AcqRel);
    }

    unsafe extern "C" fn on_cancel(_context: *mut c_void) {
        CANCELLED.fetch_add(1, Ordering::AcqRel);
    }

    fn wait_for(cond: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "timed out");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// A pipe with bytes in it fires a read source once it is resumed, and
    /// the handler sees how many bytes are waiting.
    #[test]
    fn a_read_source_fires_when_its_descriptor_is_readable() {
        let mut fds = [0; 2];
        // SAFETY: room for two descriptors.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let queue = queue::create_serial("read source test");
        // SAFETY: entry points called as a guest would.
        unsafe {
            let s = create(type_read(), fds[0] as u64, 0, queue);
            object::set_context(s, s as *mut c_void);
            set_event_handler_f(s, on_event as *const () as u64);
            set_cancel_handler_f(s, on_cancel as *const () as u64);
            libc::write(fds[1], b"hello".as_ptr().cast(), 5);
            assert_eq!(FIRED.load(Ordering::Acquire), 0, "suspended until resumed");
            resume(s);
            wait_for(|| FIRED.load(Ordering::Acquire) >= 1);
            assert!(SEEN.load(Ordering::Acquire) >= 5);
            cancel(s);
            wait_for(|| CANCELLED.load(Ordering::Acquire) == 1);
            object::release(s);
            object::release(queue);
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    static ADDED: AtomicU64 = AtomicU64::new(0);

    unsafe extern "C" fn on_add(context: *mut c_void) {
        ADDED.fetch_add(unsafe { get_data(context as u64) }, Ordering::AcqRel);
    }

    /// Merged data coalesces and is delivered as the sum.
    #[test]
    fn data_add_sums_what_was_merged() {
        let queue = queue::create_serial("data source test");
        // SAFETY: entry points called as a guest would.
        unsafe {
            let s = create(type_data_add(), 0, 0, queue);
            object::set_context(s, s as *mut c_void);
            set_event_handler_f(s, on_add as *const () as u64);
            merge_data(s, 2);
            merge_data(s, 3);
            resume(s);
            wait_for(|| ADDED.load(Ordering::Acquire) == 5);
            cancel(s);
            object::release(s);
            object::release(queue);
        }
    }

    #[test]
    fn an_unknown_type_is_refused() {
        // SAFETY: the type is checked before anything is read.
        assert_eq!(unsafe { create(0x1234, 0, 0, 0) }, 0);
    }
}
