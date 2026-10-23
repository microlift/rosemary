use std::any::TypeId;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::base::CFTypeRef;

/// `CFRunLoopSourceContext::perform` and the timer's callback: a C function this
/// process can call.
///
/// For a guest's own callback this is a trampoline `rosemary` compiled - see
/// `shims::corefoundation` - so nothing here ever holds a guest address.
pub type Perform = unsafe extern "C" fn(*mut c_void);
pub type TimerCallback = unsafe extern "C" fn(CFTypeRef, *mut c_void);

/// A version-0 source: a flag, and what to do when it is set.
pub struct Source {
    signalled: AtomicBool,
    perform: Option<Perform>,
    info: usize,
    /// Set when the source is invalidated, after which the loop drops it.
    valid: AtomicBool,
}

// SAFETY: the callback is a code address that outlives the process and `info` is
// an opaque number this crate never dereferences - it is handed back to the
// callback, which is the only thing that knows what it is.
unsafe impl Send for Source {}
unsafe impl Sync for Source {}

impl Source {
    pub fn signal(&self) {
        self.signalled.store(true, Ordering::Release);
    }

    pub fn invalidate(&self) {
        self.valid.store(false, Ordering::Release);
    }

    pub fn is_valid(&self) -> bool {
        self.valid.load(Ordering::Acquire)
    }

    /// Run the callback if the source was signalled, clearing the flag first.
    ///
    /// Cleared *before* the callback, so that a callback which signals its own
    /// source runs again next time round rather than never.
    fn fire(&self) -> bool {
        if !self.signalled.swap(false, Ordering::AcqRel) || !self.is_valid() {
            return false;
        }
        let Some(perform) = self.perform else {
            return false;
        };
        // SAFETY: `perform` is a `void (*)(void *)` the creator supplied, and
        // `info` is the pointer it asked to be given back.
        unsafe { perform(self.info as *mut c_void) };
        true
    }
}

pub struct Timer {
    next: Mutex<Instant>,
    interval: Option<Duration>,
    callback: Option<TimerCallback>,
    info: usize,
    valid: AtomicBool,
}

// SAFETY: as [`Source`].
unsafe impl Send for Timer {}
unsafe impl Sync for Timer {}

impl Timer {
    pub fn invalidate(&self) {
        self.valid.store(false, Ordering::Release);
    }

    pub fn is_valid(&self) -> bool {
        self.valid.load(Ordering::Acquire)
    }

    /// The `info` word the timer was created with.
    pub fn info(&self) -> usize {
        self.info
    }

    /// The repeat interval in seconds, or zero for a one-shot - which is how
    /// Apple's reports one, and what a caller testing "does this repeat" reads.
    pub fn interval_seconds(&self) -> f64 {
        self.interval.map_or(0.0, |i| i.as_secs_f64())
    }

    /// Move the next fire date, as `CFRunLoopTimerSetNextFireDate` does.
    ///
    /// A deadline already past fires on the next turn of the loop rather than
    /// being refused: that is what a caller asking for "now" means.
    pub fn set_next(&self, when: Instant) {
        *self.next.lock().unwrap_or_else(|e| e.into_inner()) = when;
    }

    /// When this timer is next due, or `None` if it never is.
    fn due(&self) -> Option<Instant> {
        self.is_valid()
            .then(|| *self.next.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Fire if due, and schedule the next one.
    ///
    /// A repeating timer whose interval has passed several times over is fired
    /// *once* and rescheduled from now, which is what Apple's does: catching up
    /// on a backlog of frames is never what a caller wanted.
    fn fire(self: &Arc<Timer>, now: Instant) -> bool {
        {
            let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
            if !self.is_valid() || now < *next {
                return false;
            }
            match self.interval {
                Some(interval) => *next = now + interval,
                None => {
                    self.valid.store(false, Ordering::Release);
                }
            }
        }
        let Some(callback) = self.callback else {
            return false;
        };
        let handle = Arc::as_ptr(self) as CFTypeRef;
        // SAFETY: `callback` is a `void (*)(CFRunLoopTimerRef, void *)` the
        // creator supplied, and `info` is the pointer it asked for.
        unsafe { callback(handle, self.info as *mut c_void) };
        true
    }
}

/// `CFRunLoopObserverCallBack`.
pub type ObserverCallback = unsafe extern "C" fn(CFTypeRef, u32, *mut c_void);

/// `CFRunLoopActivity`. Apple's bit values, because a caller passes a mask of them
/// in and reads one back out in the callback.
pub mod activity {
    pub const ENTRY: u32 = 1 << 0;
    pub const BEFORE_TIMERS: u32 = 1 << 1;
    pub const BEFORE_SOURCES: u32 = 1 << 2;
    pub const BEFORE_WAITING: u32 = 1 << 5;
    pub const AFTER_WAITING: u32 = 1 << 6;
    pub const EXIT: u32 = 1 << 7;
}

/// An observer: a callback and the points in the loop it wants to hear about.
///
/// This is how a toolkit that is not *driving* the loop still gets to run on every
/// turn of it - Qt's Cocoa event dispatcher hangs its `processEvents` on
/// `BeforeWaiting`, so an observer that is never called is a Qt application whose
/// events are never delivered.
pub struct Observer {
    activities: u32,
    repeats: bool,
    /// Lower runs first among observers of the same activity, which is the whole
    /// of what `order` means.
    order: i32,
    callback: Option<ObserverCallback>,
    info: usize,
    valid: AtomicBool,
}

// SAFETY: as [`Source`].
unsafe impl Send for Observer {}
unsafe impl Sync for Observer {}

impl Observer {
    pub fn invalidate(&self) {
        self.valid.store(false, Ordering::Release);
    }

    pub fn is_valid(&self) -> bool {
        self.valid.load(Ordering::Acquire)
    }

    pub fn order(&self) -> i32 {
        self.order
    }

    /// Call the callback if this observer asked about `what`.
    ///
    /// A one-shot observer is invalidated *before* the callback, so that a callback
    /// which re-adds it gets an observer that fires again rather than one already
    /// spent.
    fn fire(self: &Arc<Observer>, what: u32) {
        if !self.is_valid() || self.activities & what == 0 {
            return;
        }
        if !self.repeats {
            self.valid.store(false, Ordering::Release);
        }
        let Some(callback) = self.callback else {
            return;
        };
        let handle = Arc::as_ptr(self) as CFTypeRef;
        // SAFETY: `callback` is a
        // `void (*)(CFRunLoopObserverRef, CFRunLoopActivity, void *)` the creator
        // supplied, and `info` is the pointer it asked to be given back.
        unsafe { callback(handle, what, self.info as *mut c_void) };
    }
}

struct Common {
    names: Vec<String>,
    items: Mode,
}

#[derive(Default)]
struct Mode {
    sources: Vec<Arc<Source>>,
    timers: Vec<Arc<Timer>>,
    observers: Vec<Arc<Observer>>,
}

pub struct RunLoop {
    modes: Mutex<BTreeMap<String, Mode>>,
    /// What `kCFRunLoopCommonModes` stands for: the modes in the set, and every
    /// item added under the alias, which a mode joining the set later receives.
    ///
    /// Never held while `modes` is locked by the same call.
    common: Mutex<Common>,
    stopped: AtomicBool,
    waker: (Mutex<bool>, Condvar),
    /// Set while the loop is asleep, for `CFRunLoopIsWaiting`. A caller asks it
    /// from *another* thread to decide whether it needs to wake this one.
    waiting: AtomicBool,
    /// The modes of the runs in progress, innermost last: a modal or tracking
    /// loop runs inside the default one, and `CFRunLoopCopyCurrentMode` names
    /// the inner.
    running: Mutex<Vec<String>>,
}

pub const DEFAULT_MODE: &str = "kCFRunLoopDefaultMode";
pub const COMMON_MODES: &str = "kCFRunLoopCommonModes";

pub mod result {
    pub const FINISHED: i32 = 1;
    pub const STOPPED: i32 = 2;
    pub const TIMED_OUT: i32 = 3;
    pub const HANDLED_SOURCE: i32 = 4;
}

impl RunLoop {
    fn new() -> RunLoop {
        RunLoop {
            modes: Mutex::new(BTreeMap::new()),
            common: Mutex::new(Common {
                names: vec![DEFAULT_MODE.to_string()],
                items: Mode::default(),
            }),
            stopped: AtomicBool::new(false),
            waker: (Mutex::new(false), Condvar::new()),
            waiting: AtomicBool::new(false),
            running: Mutex::new(Vec::new()),
        }
    }

    /// The mode the innermost run in progress is in, or `None` when the loop
    /// is not running.
    pub fn current_mode(&self) -> Option<String> {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .cloned()
    }

    /// The modes a name refers to: itself, or the common set for the alias.
    fn names(&self, mode: &str) -> Vec<String> {
        if mode != COMMON_MODES {
            return vec![mode.to_string()];
        }
        self.common
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .names
            .clone()
    }

    /// Record or forget an item added under the common alias, so a mode added to
    /// the set later gets it too.
    fn note_common(&self, mode: &str, update: impl FnOnce(&mut Mode)) {
        if mode == COMMON_MODES {
            update(&mut self.common.lock().unwrap_or_else(|e| e.into_inner()).items);
        }
    }

    pub fn add_source(&self, source: &Arc<Source>, mode: &str) {
        self.note_common(mode, |common| {
            if !common.sources.iter().any(|s| Arc::ptr_eq(s, source)) {
                common.sources.push(Arc::clone(source));
            }
        });
        for name in self.names(mode) {
            let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            let entry = modes.entry(name).or_default();
            if !entry.sources.iter().any(|s| Arc::ptr_eq(s, source)) {
                entry.sources.push(Arc::clone(source));
            }
        }
        self.wake();
    }

    pub fn remove_source(&self, source: &Arc<Source>, mode: &str) {
        self.note_common(mode, |common| {
            common.sources.retain(|s| !Arc::ptr_eq(s, source));
        });
        for name in self.names(mode) {
            let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = modes.get_mut(&name) {
                entry.sources.retain(|s| !Arc::ptr_eq(s, source));
            }
        }
    }

    pub fn contains_source(&self, source: &Arc<Source>, mode: &str) -> bool {
        let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        self.names(mode).iter().any(|name| {
            modes
                .get(name)
                .is_some_and(|m| m.sources.iter().any(|s| Arc::ptr_eq(s, source)))
        })
    }

    pub fn add_timer(&self, timer: &Arc<Timer>, mode: &str) {
        self.note_common(mode, |common| {
            if !common.timers.iter().any(|t| Arc::ptr_eq(t, timer)) {
                common.timers.push(Arc::clone(timer));
            }
        });
        for name in self.names(mode) {
            let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            let entry = modes.entry(name).or_default();
            if !entry.timers.iter().any(|t| Arc::ptr_eq(t, timer)) {
                entry.timers.push(Arc::clone(timer));
            }
        }
        self.wake();
    }

    pub fn remove_timer(&self, timer: &Arc<Timer>, mode: &str) {
        self.note_common(mode, |common| {
            common.timers.retain(|t| !Arc::ptr_eq(t, timer));
        });
        for name in self.names(mode) {
            let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = modes.get_mut(&name) {
                entry.timers.retain(|t| !Arc::ptr_eq(t, timer));
            }
        }
    }

    pub fn add_observer(&self, observer: &Arc<Observer>, mode: &str) {
        self.note_common(mode, |common| {
            if !common.observers.iter().any(|o| Arc::ptr_eq(o, observer)) {
                common.observers.push(Arc::clone(observer));
            }
        });
        for name in self.names(mode) {
            let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            let entry = modes.entry(name).or_default();
            if !entry.observers.iter().any(|o| Arc::ptr_eq(o, observer)) {
                entry.observers.push(Arc::clone(observer));
                // Sorted on insertion rather than at every activity: the list
                // changes rarely and is walked several times per turn of the loop.
                entry.observers.sort_by_key(|o| o.order());
            }
        }
        self.wake();
    }

    pub fn remove_observer(&self, observer: &Arc<Observer>, mode: &str) {
        self.note_common(mode, |common| {
            common.observers.retain(|o| !Arc::ptr_eq(o, observer));
        });
        for name in self.names(mode) {
            let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = modes.get_mut(&name) {
                entry.observers.retain(|o| !Arc::ptr_eq(o, observer));
            }
        }
    }

    pub fn contains_observer(&self, observer: &Arc<Observer>, mode: &str) -> bool {
        let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        self.names(mode).iter().any(|name| {
            modes
                .get(name)
                .is_some_and(|m| m.observers.iter().any(|o| Arc::ptr_eq(o, observer)))
        })
    }

    /// Add `mode` to the set `kCFRunLoopCommonModes` stands for.
    ///
    /// As CoreFoundation does, the mode also gets everything already added under
    /// the alias: AppKit makes its modal and tracking modes common after a toolkit
    /// may have scheduled its timers and sources, and those still have to fire
    /// while a modal session runs.
    pub fn add_common_mode(&self, mode: &str) {
        if mode == COMMON_MODES {
            return;
        }
        let (sources, timers, observers) = {
            let mut common = self.common.lock().unwrap_or_else(|e| e.into_inner());
            if common.names.iter().any(|name| name == mode) {
                return;
            }
            common.names.push(mode.to_string());
            let items = &common.items;
            (
                items.sources.clone(),
                items.timers.clone(),
                items.observers.clone(),
            )
        };
        for source in &sources {
            self.add_source(source, mode);
        }
        for timer in &timers {
            self.add_timer(timer, mode);
        }
        for observer in &observers {
            self.add_observer(observer, mode);
        }
        let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        modes.entry(mode.to_string()).or_default();
    }

    /// Whether the loop is asleep waiting for something to happen.
    pub fn is_waiting(&self) -> bool {
        self.waiting.load(Ordering::Acquire)
    }

    /// Tell every observer of `what` in `mode` that it has happened.
    ///
    /// Copied out from under the lock for [`RunLoop::contents`]' reason: an
    /// observer's callback is entitled to add and remove observers, and Qt's does.
    fn observe(&self, mode: &str, what: u32) {
        let observers: Vec<Arc<Observer>> = {
            let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            let mut all: Vec<Arc<Observer>> = self
                .names(mode)
                .iter()
                .filter_map(|name| modes.get(name))
                .flat_map(|m| m.observers.iter().cloned())
                .collect();
            all.sort_by_key(|o| o.order());
            all
        };
        for observer in &observers {
            observer.fire(what);
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.wake();
    }

    /// Interrupt a loop that is waiting, from any thread.
    pub fn wake(&self) {
        let (lock, condition) = &self.waker;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        condition.notify_all();
    }

    /// Whether `mode` has anything at all to service.
    ///
    /// `CFRunLoopRunInMode` on an empty mode returns immediately rather than
    /// waiting out its timeout: a loop with no sources is one nothing can ever
    /// wake, and sleeping in it is indistinguishable from a hang.
    pub fn is_empty(&self, mode: &str) -> bool {
        let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        self.names(mode).iter().all(|name| {
            modes.get(name).is_none_or(|m| {
                m.sources.iter().all(|s| !s.is_valid()) && m.timers.iter().all(|t| !t.is_valid())
            })
        })
    }

    /// Whether `mode` holds an observer that is still valid.
    ///
    /// Asked by [`RunLoop::is_empty`]: a mode with nothing but an observer in it is
    /// not empty, because the observer is the thing that runs. Leaving it out is
    /// what would make `CFRunLoopRunInMode` return `kCFRunLoopRunFinished` to a
    /// caller whose only reason for running the loop was to be called back.
    fn has_observer(&self, mode: &str) -> bool {
        let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        self.names(mode).iter().any(|name| {
            modes
                .get(name)
                .is_some_and(|m| m.observers.iter().any(|o| o.is_valid()))
        })
    }

    /// Everything `mode` holds, copied out so the lock is not held across a
    /// callback.
    ///
    /// A callback is entitled to add or remove sources - `[NSApp run]`'s does,
    /// every frame - and holding the lock across one is a deadlock the first
    /// time it happens.
    fn contents(&self, mode: &str) -> (Vec<Arc<Source>>, Vec<Arc<Timer>>) {
        let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        let mut sources = Vec::new();
        let mut timers = Vec::new();
        for name in self.names(mode) {
            if let Some(entry) = modes.get(&name) {
                sources.extend(entry.sources.iter().cloned());
                timers.extend(entry.timers.iter().cloned());
            }
        }
        (sources, timers)
    }

    /// When the soonest valid timer in `mode` is due, if any is.
    pub fn next_timer(&self, mode: &str) -> Option<Instant> {
        self.contents(mode).1.iter().filter_map(|t| t.due()).min()
    }

    /// Run `mode` for at most `seconds`, as `CFRunLoopRunInMode` does.
    pub fn run_in_mode(&self, mode: &str, seconds: f64, return_after_source: bool) -> i32 {
        /// Pops the mode [`RunLoop::run_in_mode`] pushed, on every way out.
        struct Running<'a>(&'a Mutex<Vec<String>>);
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                self.0.lock().unwrap_or_else(|e| e.into_inner()).pop();
            }
        }
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(mode.to_string());
        let _running = Running(&self.running);
        self.stopped.store(false, Ordering::Release);
        self.observe(mode, activity::ENTRY);
        let deadline = match seconds.is_finite() && seconds >= 0.0 {
            true => Instant::now().checked_add(Duration::from_secs_f64(seconds.min(1e9))),
            false => None,
        };

        loop {
            if self.stopped.swap(false, Ordering::AcqRel) {
                self.observe(mode, activity::EXIT);
                return result::STOPPED;
            }
            if self.is_empty(mode) && !self.has_observer(mode) {
                self.observe(mode, activity::EXIT);
                return result::FINISHED;
            }

            let now = Instant::now();
            let (sources, timers) = self.contents(mode);
            let mut handled = false;
            crate::socket::poll();
            self.observe(mode, activity::BEFORE_TIMERS);
            // A fired timer is not a handled *source*: `returnAfterSourceHandled`
            // concerns sources alone, and a loop whose timer is always due (tao's
            // `ControlFlow::Poll`) must still reach `BeforeWaiting`, where toolkits
            // drain the work other threads queued for them.
            for timer in &timers {
                timer.fire(now);
            }
            self.observe(mode, activity::BEFORE_SOURCES);
            for source in &sources {
                handled |= source.fire();
            }
            if handled && return_after_source {
                self.observe(mode, activity::EXIT);
                return result::HANDLED_SOURCE;
            }
            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                self.observe(mode, activity::EXIT);
                return result::TIMED_OUT;
            }

            // Sleep until the next timer, the deadline, or a wake-up - whichever
            // comes first. A loop that spun here would be a busy wait in every
            // program that idles, which is most of them.
            let next_timer = timers.iter().filter_map(|t| t.due()).min();
            let wait_until = [next_timer, deadline]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or_else(|| Instant::now() + Duration::from_millis(50));
            // `BeforeWaiting` is the activity a toolkit does its own work in, so it
            // is announced before the sleep and `AfterWaiting` after it - and an
            // observer that signals a source from here is why the sleep may end up
            // being no sleep at all.
            self.observe(mode, activity::BEFORE_WAITING);
            self.waiting.store(true, Ordering::Release);
            self.wait_until(wait_until);
            self.waiting.store(false, Ordering::Release);
            self.observe(mode, activity::AFTER_WAITING);
        }
    }

    fn wait_until(&self, deadline: Instant) {
        let (lock, condition) = &self.waker;
        let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());
        if *woken {
            *woken = false;
            return;
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        let (mut woken, _) = condition
            .wait_timeout(woken, timeout)
            .unwrap_or_else(|e| e.into_inner());
        *woken = false;
    }
}

/// Build an observer.
pub fn new_observer(
    activities: u32,
    repeats: bool,
    order: i32,
    callback: Option<ObserverCallback>,
    info: usize,
) -> Arc<Observer> {
    let observer = Arc::new(Observer {
        activities,
        repeats,
        order,
        callback,
        info,
        valid: AtomicBool::new(true),
    });
    note(Arc::as_ptr(&observer));
    observer
}

/// Build a version-0 source.
pub fn new_source(perform: Option<Perform>, info: usize) -> Arc<Source> {
    let source = Arc::new(Source {
        signalled: AtomicBool::new(false),
        perform,
        info,
        valid: AtomicBool::new(true),
    });
    note(Arc::as_ptr(&source));
    source
}

/// Every run loop, source and timer this module made, and which of the three it
/// is.
///
/// These are the one family of CoreFoundation handles that is *not* an object
/// with an isa - a `CFRunLoopSourceRef` here is a leaked `Arc` - so nothing
/// about the pointer says what it points at, and the isa check
/// [`crate::foreign`] uses cannot be asked. It has to be asked of something: a
/// program under the builtin frameworks still gets sources from Apple's own
/// libraries, IOKit's notification port among them, and one of those read as a
/// [`Source`] turns a field of a structure this crate did not lay out into the
/// callback the loop then calls.
static HANDLES: Mutex<BTreeMap<usize, TypeId>> = Mutex::new(BTreeMap::new());

/// Record a handle before it is handed to the C world.
pub fn note<T: 'static>(handle: *const T) {
    HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(handle as usize, TypeId::of::<T>());
    // Run loops, sources and timers are CoreFoundation references even though
    // this implementation represents them as Rust allocations without an
    // Objective-C isa. Make the shared retain/release path recognize them too;
    // otherwise Darwin mistakes them for host CF objects and forwards them to
    // Apple's CoreFoundation.
    crate::base::note_opaque(handle.cast());
}

/// Whether `handle` is a `T` this module handed out.
pub fn is_noted<T: 'static>(handle: usize) -> bool {
    HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&handle)
        == Some(&TypeId::of::<T>())
}

/// Build a timer, first due `after` from now and repeating every `interval`.
pub fn new_timer(
    after: Duration,
    interval: Option<Duration>,
    callback: Option<TimerCallback>,
    info: usize,
) -> Arc<Timer> {
    let timer = Arc::new(Timer {
        next: Mutex::new(Instant::now() + after),
        interval,
        callback,
        info,
        valid: AtomicBool::new(true),
    });
    note(Arc::as_ptr(&timer));
    timer
}

thread_local! {
    /// This thread's run loop, made on the first `CFRunLoopGetCurrent`.
    static CURRENT: std::cell::OnceCell<Arc<RunLoop>> = const { std::cell::OnceCell::new() };
}

/// The main thread's, which `CFRunLoopGetMain` answers from any thread.
static MAIN: Mutex<Option<Arc<RunLoop>>> = Mutex::new(None);

/// This thread's run loop, made if it has none.
///
/// The main thread's is the one [`main`] answers, whoever asked for it first.
/// "First caller" is not the main thread: Unity's joystick thread asks for its
/// own loop before the main thread does, and main-loop timers then fired there.
pub fn current() -> Arc<RunLoop> {
    CURRENT.with(|cell| {
        Arc::clone(cell.get_or_init(|| {
            // SAFETY: no arguments; answers from thread ids.
            match unsafe { darwin::pthread::main_np() } != 0 {
                true => main(),
                false => fresh(),
            }
        }))
    })
}

/// The main thread's run loop, made if nothing has asked for it yet.
pub fn main() -> Arc<RunLoop> {
    let mut main = MAIN.lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(main.get_or_insert_with(fresh))
}

fn fresh() -> Arc<RunLoop> {
    let loop_ = Arc::new(RunLoop::new());
    note(Arc::as_ptr(&loop_));
    loop_
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A counter and a pair of callbacks, per test.
    ///
    /// Per test rather than shared: the suite runs them on several threads at
    /// once, and one counter between them makes every assertion here depend on
    /// which other test happened to be running.
    macro_rules! counter {
        () => {{
            static CALLS: AtomicUsize = AtomicUsize::new(0);
            unsafe extern "C" fn source(_info: *mut c_void) {
                CALLS.fetch_add(1, Ordering::SeqCst);
            }
            unsafe extern "C" fn timer(_timer: CFTypeRef, _info: *mut c_void) {
                CALLS.fetch_add(1, Ordering::SeqCst);
            }
            (
                || CALLS.load(Ordering::SeqCst),
                source as Perform,
                timer as TimerCallback,
            )
        }};
    }

    /// Unity's joystick thread asks for its own loop before the main thread
    /// asks for anything, and main-loop timers then fired on it.
    #[test]
    fn another_threads_loop_is_never_the_main_one() {
        // Neither the harness's thread nor a spawned one is the main thread.
        let theirs = std::thread::spawn(current).join().unwrap();
        let main = main();
        assert!(!Arc::ptr_eq(&main, &theirs));
        assert!(!Arc::ptr_eq(&main, &current()));
        assert!(Arc::ptr_eq(&main, &super::main()));
    }

    #[test]
    fn a_signalled_source_is_serviced_and_a_quiet_one_is_not() {
        let (calls, perform, _) = counter!();
        let loop_ = Arc::new(RunLoop::new());
        let source = new_source(Some(perform), 0);
        loop_.add_source(&source, DEFAULT_MODE);

        // Nothing signalled: the loop waits out its timeout and says so.
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 0.01, true),
            result::TIMED_OUT
        );
        assert_eq!(calls(), 0);

        source.signal();
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 1.0, true),
            result::HANDLED_SOURCE
        );
        assert_eq!(calls(), 1);
        // Signalled once, serviced once: the flag is cleared by the firing.
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 0.01, true),
            result::TIMED_OUT
        );
        assert_eq!(calls(), 1);
    }

    #[test]
    fn the_next_timer_is_the_soonest_valid_one_in_the_mode() {
        let (_, _, tick) = counter!();
        let loop_ = Arc::new(RunLoop::new());
        assert_eq!(loop_.next_timer(DEFAULT_MODE), None, "nothing scheduled");

        let soon = new_timer(Duration::from_secs(10), None, Some(tick), 0);
        let later = new_timer(Duration::from_secs(20), None, Some(tick), 0);
        loop_.add_timer(&later, DEFAULT_MODE);
        loop_.add_timer(&soon, DEFAULT_MODE);
        let due = soon.due().expect("a valid timer is due");
        assert_eq!(loop_.next_timer(DEFAULT_MODE), Some(due));
        assert_eq!(loop_.next_timer("other"), None, "another mode is not asked");

        soon.invalidate();
        assert_eq!(loop_.next_timer(DEFAULT_MODE), later.due());
    }

    /// A counter that also records which activities it was told about.
    macro_rules! observer_counter {
        () => {{
            static SEEN: Mutex<Vec<u32>> = Mutex::new(Vec::new());
            unsafe extern "C" fn observe(_observer: CFTypeRef, what: u32, _info: *mut c_void) {
                SEEN.lock().unwrap_or_else(|e| e.into_inner()).push(what);
            }
            (
                || SEEN.lock().unwrap_or_else(|e| e.into_inner()).clone(),
                observe as ObserverCallback,
            )
        }};
    }

    /// The activities a turn of the loop announces, in order. This is the contract
    /// an event dispatcher built on observers depends on.
    #[test]
    fn a_turn_of_the_loop_announces_its_activities_in_order() {
        let (seen, observe) = observer_counter!();
        let loop_ = Arc::new(RunLoop::new());
        let observer = new_observer(
            activity::ENTRY
                | activity::BEFORE_TIMERS
                | activity::BEFORE_SOURCES
                | activity::BEFORE_WAITING
                | activity::AFTER_WAITING
                | activity::EXIT,
            true,
            0,
            Some(observe),
            0,
        );
        loop_.add_observer(&observer, DEFAULT_MODE);
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 0.01, false),
            result::TIMED_OUT,
            "an observer is something to service, so the mode is not finished"
        );
        let seen = seen();
        assert_eq!(
            seen.first(),
            Some(&activity::ENTRY),
            "entry first: {seen:?}"
        );
        assert_eq!(seen.last(), Some(&activity::EXIT), "exit last: {seen:?}");
        let first_turn: Vec<u32> = seen
            .iter()
            .copied()
            .skip(1)
            .take_while(|a| *a != activity::EXIT)
            .take(4)
            .collect();
        assert_eq!(
            first_turn,
            [
                activity::BEFORE_TIMERS,
                activity::BEFORE_SOURCES,
                activity::BEFORE_WAITING,
                activity::AFTER_WAITING
            ],
            "{seen:?}"
        );
    }

    /// A one-shot observer fires once however many turns the loop takes, and an
    /// invalidated one does not fire at all.
    #[test]
    fn a_one_shot_observer_fires_once_and_an_invalid_one_never() {
        let (seen, observe) = observer_counter!();
        let loop_ = Arc::new(RunLoop::new());
        let once = new_observer(activity::BEFORE_WAITING, false, 0, Some(observe), 0);
        loop_.add_observer(&once, DEFAULT_MODE);
        loop_.run_in_mode(DEFAULT_MODE, 0.03, false);
        assert_eq!(seen().len(), 1, "a one-shot observer fires once");
        assert!(!once.is_valid(), "and is spent");

        let never = new_observer(activity::BEFORE_WAITING, true, 0, Some(observe), 0);
        never.invalidate();
        loop_.add_observer(&never, DEFAULT_MODE);
        loop_.run_in_mode(DEFAULT_MODE, 0.03, false);
        assert_eq!(seen().len(), 1, "an invalidated observer is not called");
    }

    /// Lower order runs first, which is the whole of what `order` promises.
    #[test]
    fn observers_run_in_order() {
        static ORDER: Mutex<Vec<i32>> = Mutex::new(Vec::new());
        unsafe extern "C" fn first(_o: CFTypeRef, _w: u32, _i: *mut c_void) {
            ORDER.lock().unwrap_or_else(|e| e.into_inner()).push(1);
        }
        unsafe extern "C" fn second(_o: CFTypeRef, _w: u32, _i: *mut c_void) {
            ORDER.lock().unwrap_or_else(|e| e.into_inner()).push(2);
        }
        let loop_ = Arc::new(RunLoop::new());
        // Added high-order first, so the ordering cannot come from insertion.
        let high = new_observer(
            activity::ENTRY,
            false,
            10,
            Some(second as ObserverCallback),
            0,
        );
        let low = new_observer(
            activity::ENTRY,
            false,
            -10,
            Some(first as ObserverCallback),
            0,
        );
        loop_.add_observer(&high, DEFAULT_MODE);
        loop_.add_observer(&low, DEFAULT_MODE);
        loop_.run_in_mode(DEFAULT_MODE, 0.0, false);
        assert_eq!(
            ORDER.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
            [1, 2]
        );
    }

    /// An empty mode returns at once rather than sleeping: a loop nothing can
    /// wake is indistinguishable from a hang.
    #[test]
    fn an_empty_mode_finishes_immediately() {
        let loop_ = Arc::new(RunLoop::new());
        let started = Instant::now();
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 30.0, false),
            result::FINISHED
        );
        assert!(started.elapsed() < Duration::from_secs(1), "it waited");
    }

    #[test]
    fn a_repeating_timer_fires_more_than_once_and_stops_when_invalidated() {
        let (calls, _, callback) = counter!();
        let loop_ = Arc::new(RunLoop::new());
        let timer = new_timer(
            Duration::from_millis(1),
            Some(Duration::from_millis(1)),
            Some(callback),
            0,
        );
        loop_.add_timer(&timer, DEFAULT_MODE);
        loop_.run_in_mode(DEFAULT_MODE, 0.05, false);
        let fired = calls();
        assert!(fired >= 2, "a repeating timer fired {fired} times");

        timer.invalidate();
        // With nothing valid left, the mode is empty.
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 1.0, false),
            result::FINISHED
        );
        assert_eq!(calls(), fired, "it fired after invalidation");
    }

    /// A one-shot timer fires once and invalidates itself.
    #[test]
    fn a_one_shot_timer_fires_once() {
        let (calls, _, callback) = counter!();
        let loop_ = Arc::new(RunLoop::new());
        let timer = new_timer(Duration::from_millis(1), None, Some(callback), 0);
        loop_.add_timer(&timer, DEFAULT_MODE);
        loop_.run_in_mode(DEFAULT_MODE, 0.05, false);
        assert_eq!(calls(), 1);
        assert!(!timer.is_valid());
    }

    /// Signalled from another thread while the loop is idle: the wake-up is what
    /// makes this finish in milliseconds rather than at the end of the timeout.
    #[test]
    fn a_source_signalled_from_another_thread_wakes_the_loop() {
        let (calls, perform, _) = counter!();
        let loop_ = Arc::new(RunLoop::new());
        let source = new_source(Some(perform), 0);
        loop_.add_source(&source, DEFAULT_MODE);

        let waker = Arc::clone(&loop_);
        let signal = Arc::clone(&source);
        let started = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            signal.signal();
            waker.wake();
        });
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 30.0, true),
            result::HANDLED_SOURCE
        );
        assert_eq!(calls(), 1);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it slept the timeout out"
        );
    }

    /// `CFRunLoopStop` from inside a callback ends the loop.
    #[test]
    fn stopping_from_inside_a_callback_returns_stopped() {
        static LOOP: Mutex<Option<Arc<RunLoop>>> = Mutex::new(None);
        unsafe extern "C" fn stop_it(_info: *mut c_void) {
            if let Some(l) = LOOP.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                l.stop();
            }
        }
        let loop_ = Arc::new(RunLoop::new());
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&loop_));
        let source = new_source(Some(stop_it), 0);
        loop_.add_source(&source, DEFAULT_MODE);
        source.signal();
        assert_eq!(loop_.run_in_mode(DEFAULT_MODE, 5.0, false), result::STOPPED);
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// A source added to the common modes is run by the default one.
    #[test]
    fn the_common_modes_alias_reaches_the_default_mode() {
        let (calls, perform, _) = counter!();
        let loop_ = Arc::new(RunLoop::new());
        let source = new_source(Some(perform), 0);
        loop_.add_source(&source, COMMON_MODES);
        source.signal();
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 1.0, true),
            result::HANDLED_SOURCE
        );
        assert_eq!(calls(), 1);
    }

    /// A mode made common later still runs what was added under the alias
    /// before, and a mode that is not common does not.
    #[test]
    fn a_mode_made_common_later_gets_what_the_alias_already_holds() {
        let (calls, perform, _) = counter!();
        let loop_ = Arc::new(RunLoop::new());
        let source = new_source(Some(perform), 0);
        loop_.add_source(&source, COMMON_MODES);
        source.signal();
        assert_eq!(
            loop_.run_in_mode("Private", 0.0, true),
            result::FINISHED,
            "a mode outside the common set holds nothing"
        );

        loop_.add_common_mode("Modal");
        assert_eq!(
            loop_.run_in_mode("Modal", 1.0, true),
            result::HANDLED_SOURCE
        );
        assert_eq!(calls(), 1);

        loop_.remove_source(&source, COMMON_MODES);
        assert_eq!(loop_.run_in_mode("Modal", 0.0, true), result::FINISHED);
        assert_eq!(loop_.run_in_mode(DEFAULT_MODE, 0.0, true), result::FINISHED);
    }

    /// tao runs `ControlFlow::Poll` with a timer that is always due and drains
    /// work other threads queued for it from `BeforeWaiting`. A fired timer is not
    /// a handled source, so a run that returns after one must still get there.
    #[test]
    fn a_due_timer_does_not_end_a_return_after_source_run_early() {
        thread_local! {
            static SEEN: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }
        unsafe extern "C" fn observer(_: CFTypeRef, what: u32, _: *mut c_void) {
            if what == activity::BEFORE_WAITING {
                SEEN.with(|s| s.set(s.get() + 1));
            }
        }
        unsafe extern "C" fn timer_cb(_: CFTypeRef, _: *mut c_void) {}
        let loop_ = Arc::new(RunLoop::new());
        loop_.add_observer(
            &new_observer(activity::BEFORE_WAITING, true, 0, Some(observer), 0),
            DEFAULT_MODE,
        );
        loop_.add_timer(
            &new_timer(
                Duration::ZERO,
                Some(Duration::from_millis(1)),
                Some(timer_cb),
                0,
            ),
            DEFAULT_MODE,
        );
        loop_.run_in_mode(DEFAULT_MODE, 0.01, true);
        assert!(SEEN.with(|s| s.get()) > 0);
    }
}
