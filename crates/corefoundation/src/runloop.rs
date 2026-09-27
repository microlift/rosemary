use std::any::TypeId;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::base::{CFIndex, CFTypeRef};

/// `CFRunLoopSourceContext::perform` and the timer's callback: a C function this
/// process can call.
///
/// For a guest's own callback this is a trampoline `rosetta3` compiled - see
/// `shims::corefoundation` - so nothing here ever holds a guest address.
pub type Perform = unsafe extern "C" fn(*mut c_void);
pub type TimerCallback = unsafe extern "C" fn(CFTypeRef, *mut c_void);

/// A version-0 source: a flag, and what to do when it is set.
///
/// `repr(C)` and not the default, and that is load-bearing rather than
/// housekeeping: [`witness`] reads the **first word** of the allocation, and
/// `repr(Rust)` gives no promise about where a field lands. Measured here, not
/// assumed: with the default layout rustc put `Source::witness` at offset **32**
/// and `RunLoop::witness` at **96**, so the word the witness read was a `Mutex`'s
/// or a `Vec`'s internals and the check answered for every live handle. A test
/// pins the offsets.
#[repr(C)]
pub struct Source {
    /// Always zero, and the whole of what [`witness`] reads.
    ///
    /// The first word of this struct is the only word the run-loop handle
    /// registry knows how to check, and it is checked because a bare address
    /// cannot be: an entry left behind by a freed handle claims whatever the
    /// allocator builds there next, and `CFRetain` on a live object that lands
    /// there is a **silent no-op** whose only symptom is a retain count that
    /// never moves. A word this struct writes once and nothing else ever writes
    /// answers that exactly - a real object has its `isa` there, and a
    /// `CFMachPort` has a port name - where reading a `bool` next to seven
    /// bytes of padding would answer by luck.
    ///
    /// Cost: eight bytes per handle, and one invariant to keep. Cheaper than the
    /// alternative, which is a wrong answer.
    /// Read only through the address, by [`witness`], which is the whole point.
    #[allow(dead_code)]
    witness: usize,
    signalled: AtomicBool,
    perform: Option<Perform>,
    info: usize,
    /// The guest's own sort key: `CFRunLoopSourceCreate`'s `order` argument.
    ///
    /// "The order in which this source is processed relative to other sources in
    /// the same mode; sources with a lower order are processed first." Dropped on
    /// the floor, this is not a difference any caller can see on its own - but a
    /// loop that services sources in the order it was given them instead of the
    /// order it was told is servicing a *different* set of things per turn, and
    /// the guest's own choice of orders is evidence of what it expects first.
    order: CFIndex,
    /// The loops this source has been added to, so invalidation can take it out
    /// of every one of them.
    ///
    /// Weak, because the loop holds the strong reference and a source must not
    /// keep a loop - or a thread - alive. This is what makes
    /// [`Source::invalidate`] able to do what Apple's does: remove the source
    /// from all modes of all run loops, rather than only mark it dead and leave
    /// the loop holding a callback whose `info` the guest is about to free.
    holders: Mutex<Vec<Weak<RunLoop>>>,
    /// Set when the source is invalidated, after which the loop drops it.
    valid: AtomicBool,
    /// "Is there anything waiting for you?", asked just before `perform` runs
    /// and given the same `info` the callback is.
    ///
    /// For a source whose readiness lives somewhere this crate does not own - a
    /// Mach port's queue - and which is therefore looked at by a *process-wide*
    /// sweep rather than by the loop that owns it. That sweep can signal a source
    /// belonging to another thread's loop, and by the time that loop reaches it
    /// the port may be empty; without this the source would report a source
    /// handled for work it did not do, and `CFRunLoopRunInMode` would answer
    /// `kCFRunLoopRunHandledSource` for a turn on which nothing was delivered.
    ///
    /// A bare function taking the source's own `info` rather than a closure, so
    /// one of them serves every source of a kind and a port costs nothing.
    ready: Option<fn(usize) -> bool>,
}

// SAFETY: the callbacks are code addresses that outlive the process and `info` is
// an opaque number this crate never dereferences - it is handed back to the
// callback, which is the only thing that knows what it is.
unsafe impl Send for Source {}
unsafe impl Sync for Source {}

impl Source {
    /// The guest's `order` argument, which is the whole of what sorting means.
    pub fn order(&self) -> CFIndex {
        self.order
    }

    pub fn signal(&self) {
        self.signalled.store(true, Ordering::Release);
    }

    /// Invalidate, and take the source out of every loop that holds it.
    ///
    /// Apple's `CFRunLoopSourceInvalidate` does both halves, and the second is
    /// the one that matters for a guest's memory: the source is "removed from
    /// all run loops and modes", and a version-0 source's `info` is the guest's
    /// own pointer. A loop that keeps a dead source is a loop holding a callback
    /// address whose object the guest is entitled to have freed - which is
    /// exactly the shape a use-after-free takes, and the one thing this module
    /// must never be the reason for.
    ///
    /// Idempotent, because a second invalidation has nothing left to remove.
    pub fn invalidate(&self) {
        if !self.valid.swap(false, Ordering::AcqRel) {
            return;
        }
        let holders: Vec<Arc<RunLoop>> = {
            let mut held = self.holders.lock().unwrap_or_else(|e| e.into_inner());
            // Drop entries for loops that have gone, so a long-lived source that
            // has been through many short-lived loops does not accumulate them.
            held.retain(|weak| weak.strong_count() != 0);
            held.iter().filter_map(Weak::upgrade).collect()
        };
        for loop_ in holders {
            loop_.remove_source_everywhere(self);
        }
    }

    /// Record that `loop_` is holding this source, so invalidation can find it.
    fn note_holder(&self, loop_: &Arc<RunLoop>) {
        let here = Arc::as_ptr(loop_);
        let mut held = self.holders.lock().unwrap_or_else(|e| e.into_inner());
        if held.iter().any(|weak| Weak::as_ptr(weak) == here) {
            return;
        }
        held.retain(|weak| weak.strong_count() != 0);
        held.push(Arc::downgrade(loop_));
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
        // A source that says nothing is waiting for it has nothing to deliver,
        // however it came to be signalled. See `Source::ready`.
        if self.ready.is_some_and(|ready| !ready(self.info)) {
            return false;
        }
        // SAFETY: `perform` is a `void (*)(void *)` the creator supplied, and
        // `info` is the pointer it asked to be given back.
        unsafe { perform(self.info as *mut c_void) };
        true
    }
}

#[repr(C)]
pub struct Timer {
    /// Always zero; see [`Source::witness`].
    /// Read only through the address, by [`witness`], which is the whole point.
    #[allow(dead_code)]
    witness: usize,
    next: Mutex<Instant>,
    interval: Option<Duration>,
    callback: Option<TimerCallback>,
    info: usize,
    /// The guest's own sort key, `CFRunLoopTimerCreate`'s `order` argument.
    order: CFIndex,
    /// The loops holding this timer, so invalidation can take it out of them.
    holders: Mutex<Vec<Weak<RunLoop>>>,
    valid: AtomicBool,
    /// The window past the fire date within which firing is still correct.
    ///
    /// Apple's header: "The timer may fire at any time between its scheduled
    /// fire date and the scheduled fire date plus the tolerance. The timer will
    /// not fire before the scheduled fire date. ... The default value is zero,
    /// which means no additional tolerance is applied."
    ///
    /// This run loop fires every timer at its exact fire date, which is inside
    /// that window for any tolerance at all, so the value is never *needed* -
    /// it is kept because it is what the caller asked for and because the
    /// invariant it states is checkable: see [`Timer::tolerance`].
    tolerance: Mutex<f64>,
}

// SAFETY: as [`Source`].
unsafe impl Send for Timer {}
unsafe impl Sync for Timer {}

impl Timer {
    /// The guest's `order` argument.
    pub fn order(&self) -> CFIndex {
        self.order
    }

    /// Invalidate, and take the timer out of every loop that holds it.
    ///
    /// As [`Source::invalidate`]: a timer whose `info` is the guest's and which
    /// a loop still holds is a callback into memory the guest may have freed, and
    /// it is also what keeps a mode from ever being reported finished.
    pub fn invalidate(&self) {
        if !self.valid.swap(false, Ordering::AcqRel) {
            return;
        }
        let holders: Vec<Arc<RunLoop>> = {
            let mut held = self.holders.lock().unwrap_or_else(|e| e.into_inner());
            held.retain(|weak| weak.strong_count() != 0);
            held.iter().filter_map(Weak::upgrade).collect()
        };
        for loop_ in holders {
            loop_.remove_timer_everywhere(self);
        }
    }

    /// Record that `loop_` is holding this timer, so invalidation can find it.
    fn note_holder(&self, loop_: &Arc<RunLoop>) {
        let here = Arc::as_ptr(loop_);
        let mut held = self.holders.lock().unwrap_or_else(|e| e.into_inner());
        if held.iter().any(|weak| Weak::as_ptr(weak) == here) {
            return;
        }
        held.retain(|weak| weak.strong_count() != 0);
        held.push(Arc::downgrade(loop_));
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

    /// Record the tolerance `CFRunLoopTimerSetTolerance` was given, clamped the
    /// way `CFRunLoop.c` clamps it.
    ///
    /// Apple's own words for the two arms, from `CFRunLoopTimerSetTolerance`:
    ///
    /// ```c
    /// if (rlt->_interval > 0) {
    ///     rlt->_tolerance = MIN(tolerance, rlt->_interval / 2);
    /// } else {
    ///     if (tolerance < 0) tolerance = 0.0;
    ///     rlt->_tolerance = tolerance;
    /// }
    /// ```
    ///
    /// A repeating timer is limited to half an interval so a tolerance can never
    /// reach past the timer's own next fire, and a one-shot is limited to zero
    /// because a negative tolerance would authorise firing *before* the date,
    /// which the header forbids outright.
    pub fn set_tolerance(&self, tolerance: f64) {
        let clamped = match self.interval {
            Some(interval) => tolerance.min(interval.as_secs_f64() / 2.0),
            None => tolerance.max(0.0),
        };
        *self.tolerance.lock().unwrap_or_else(|e| e.into_inner()) = clamped;
    }

    /// The tolerance in force, in seconds.
    ///
    /// The invariant this run loop keeps, and the reason the value is worth
    /// keeping: a timer fires at `next`, never later than `next + tolerance`, and
    /// `next` itself is always inside that window.
    pub fn tolerance(&self) -> f64 {
        *self.tolerance.lock().unwrap_or_else(|e| e.into_inner())
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
#[repr(C)]
pub struct Observer {
    /// Always zero; see [`Source::witness`].
    /// Read only through the address, by [`witness`], which is the whole point.
    #[allow(dead_code)]
    witness: usize,
    activities: u32,
    repeats: bool,
    /// Lower runs first among observers of the same activity, which is the whole
    /// of what `order` means.
    order: i32,
    callback: Option<ObserverCallback>,
    info: usize,
    /// The loops holding this observer, so invalidation can take it out of them.
    holders: Mutex<Vec<Weak<RunLoop>>>,
    valid: AtomicBool,
}

// SAFETY: as [`Source`].
unsafe impl Send for Observer {}
unsafe impl Sync for Observer {}

impl Observer {
    /// Invalidate, and take the observer out of every loop that holds it.
    ///
    /// As [`Source::invalidate`]. An observer's `info` is the guest's too, and
    /// an observer is the one item here whose *only* job is to be called - so
    /// leaving a dead one in a loop is the surest way to call into freed memory.
    pub fn invalidate(&self) {
        if !self.valid.swap(false, Ordering::AcqRel) {
            return;
        }
        let holders: Vec<Arc<RunLoop>> = {
            let mut held = self.holders.lock().unwrap_or_else(|e| e.into_inner());
            held.retain(|weak| weak.strong_count() != 0);
            held.iter().filter_map(Weak::upgrade).collect()
        };
        for loop_ in holders {
            loop_.remove_observer_everywhere(self);
        }
    }

    /// Record that `loop_` is holding this observer, so invalidation can find it.
    fn note_holder(&self, loop_: &Arc<RunLoop>) {
        let here = Arc::as_ptr(loop_);
        let mut held = self.holders.lock().unwrap_or_else(|e| e.into_inner());
        if held.iter().any(|weak| Weak::as_ptr(weak) == here) {
            return;
        }
        held.retain(|weak| weak.strong_count() != 0);
        held.push(Arc::downgrade(loop_));
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

/// Work `CFRunLoopPerformBlock` has queued, waiting for the loop to reach a
/// mode it is scheduled in.
///
/// Not a source and not a timer, because a block is neither: it is not added to
/// a mode, not signalled, and not invalidated, and it runs *once*. The list is
/// per-loop rather than per-mode precisely because the mode a block is scheduled
/// for is resolved when the loop gets there - so a mode made common after the
/// block was queued under the alias runs it, exactly as it runs the sources that
/// were added under that alias before it.
struct ScheduledBlock {
    mode: String,
    run: Box<dyn FnOnce() + Send + 'static>,
}

/// One `CFRunLoopRunInMode` call, on the stack for as long as it lasts.
///
/// A run loop **re-enters**. `CFRunLoopRunInMode` is callable from inside a
/// source callback or an observer callback, and Cocoa does it constantly:
/// `-[NSApplication nextEventMatchingMask:...]` runs the default mode from
/// inside the `CFRunLoopRun` that `-[NSApplication run]` is in. So neither "what
/// mode is this loop in" nor "has it been asked to stop" can be a single value.
struct Run {
    /// The mode this invocation is in, which is `CFRunLoopCopyCurrentMode`'s
    /// answer - the *innermost* one, as Apple's is.
    mode: String,
    /// This invocation's own stop request.
    ///
    /// Per invocation rather than per loop because that is what `CFRunLoopStop`
    /// acts on: it stops "the current invocation of `CFRunLoopRunInMode` on this
    /// thread's run loop". A stop asked for inside a nested run is consumed by
    /// *that* run and the one it was nested in carries on - which is the
    /// difference between a quit and a lost quit.
    stopped: bool,
}

#[repr(C)]
pub struct RunLoop {
    /// Always zero; see [`Source::witness`].
    /// Read only through the address, by [`witness`], which is the whole point.
    #[allow(dead_code)]
    witness: usize,
    /// The thread this loop belongs to, which is the one that may turn it.
    ///
    /// A run loop belongs to one thread - that is what makes it a run loop and
    /// not a shared queue - and this field is only ever *read*, to say so out loud
    /// when something else turns it. See [`RunLoop::check_ownership`].
    owner: u32,
    modes: Mutex<BTreeMap<String, Mode>>,
    /// What `kCFRunLoopCommonModes` stands for: the modes in the set, and every
    /// item added under the alias, which a mode joining the set later receives.
    ///
    /// Never held while `modes` is locked by the same call.
    common: Mutex<Common>,
    /// The runs currently on the stack, innermost last, and empty when the loop
    /// is not running - which is what makes `CFRunLoopCopyCurrentMode` answer
    /// null and a `CFRunLoopStop` on an idle loop a no-op.
    ///
    /// A property of the loop rather than of the thread asking, which is what
    /// Apple does too: the mode belongs to the loop, and a run loop belongs to
    /// one thread, so nothing else can be the one running it.
    runs: Mutex<Vec<Run>>,
    blocks: Mutex<Vec<ScheduledBlock>>,
    waker: (Mutex<bool>, Condvar),
    /// Set while the loop is asleep, for `CFRunLoopIsWaiting`. A caller asks it
    /// from *another* thread to decide whether it needs to wake this one.
    waiting: AtomicBool,
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
            witness: 0,
            owner: darwin::pthread::id(),
            modes: Mutex::new(BTreeMap::new()),
            common: Mutex::new(Common {
                names: vec![DEFAULT_MODE.to_string()],
                items: Mode::default(),
            }),
            runs: Mutex::new(Vec::new()),
            blocks: Mutex::new(Vec::new()),
            waker: (Mutex::new(false), Condvar::new()),
            waiting: AtomicBool::new(false),
        }
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

    pub fn add_source(self: &Arc<Self>, source: &Arc<Source>, mode: &str) {
        // Apple's rule for all three kinds of item, and load-bearing rather than
        // cosmetic: adding something already invalidated does nothing. If it did
        // something, a mode would hold a dead source forever, and a mode that
        // holds a source is a mode `CFRunLoopRunInMode` must not report as
        // finished - so a thread that reached this would never come back.
        if !source.is_valid() {
            return;
        }
        source.note_holder(self);
        self.note_common(mode, |common| {
            if !common.sources.iter().any(|s| Arc::ptr_eq(s, source)) {
                common.sources.push(Arc::clone(source));
                // Stable, so items with equal `order` keep the order they were
                // added in - which is the only tie-break there is.
                common.sources.sort_by_key(|s| s.order());
            }
        });
        for name in self.names(mode) {
            let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            let entry = modes.entry(name).or_default();
            if !entry.sources.iter().any(|s| Arc::ptr_eq(s, source)) {
                entry.sources.push(Arc::clone(source));
                // Sorted on insertion rather than at every turn: the list changes
                // rarely and is walked once per turn of the loop.
                entry.sources.sort_by_key(|s| s.order());
            }
        }
        self.wake();
    }

    pub fn remove_source(self: &Arc<Self>, source: &Arc<Source>, mode: &str) {
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

    /// Take `source` out of every mode of this loop, whatever it was added to.
    ///
    /// What invalidation is entitled to: `CFRunLoopSourceInvalidate` names no
    /// mode, so it cannot be a removal from one. Called only from
    /// [`Source::invalidate`], and never while `modes` is already held.
    pub fn remove_source_everywhere(&self, source: &Source) {
        self.common
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .items
            .sources
            .retain(|s| !std::ptr::eq(Arc::as_ptr(s).cast::<Source>(), source));
        let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        for entry in modes.values_mut() {
            entry
                .sources
                .retain(|s| !std::ptr::eq(Arc::as_ptr(s).cast::<Source>(), source));
        }
    }

    pub fn contains_source(self: &Arc<Self>, source: &Arc<Source>, mode: &str) -> bool {
        // The names first, then `modes`: every other path takes `common` before
        // `modes`, and asking for the common set while holding `modes` would be
        // the one ordering that could deadlock against them.
        let names = self.names(mode);
        let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        names.iter().any(|name| {
            modes
                .get(name)
                .is_some_and(|m| m.sources.iter().any(|s| Arc::ptr_eq(s, source)))
        })
    }

    pub fn add_timer(self: &Arc<Self>, timer: &Arc<Timer>, mode: &str) {
        if !timer.is_valid() {
            return;
        }
        timer.note_holder(self);
        self.note_common(mode, |common| {
            if !common.timers.iter().any(|t| Arc::ptr_eq(t, timer)) {
                common.timers.push(Arc::clone(timer));
                common.timers.sort_by_key(|t| t.order());
            }
        });
        for name in self.names(mode) {
            let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
            let entry = modes.entry(name).or_default();
            if !entry.timers.iter().any(|t| Arc::ptr_eq(t, timer)) {
                entry.timers.push(Arc::clone(timer));
                entry.timers.sort_by_key(|t| t.order());
            }
        }
        self.wake();
    }

    pub fn remove_timer(self: &Arc<Self>, timer: &Arc<Timer>, mode: &str) {
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

    /// [`RunLoop::remove_source_everywhere`] for a timer.
    pub fn remove_timer_everywhere(&self, timer: &Timer) {
        self.common
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .items
            .timers
            .retain(|t| !std::ptr::eq(Arc::as_ptr(t).cast::<Timer>(), timer));
        let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        for entry in modes.values_mut() {
            entry
                .timers
                .retain(|t| !std::ptr::eq(Arc::as_ptr(t).cast::<Timer>(), timer));
        }
    }

    pub fn add_observer(self: &Arc<Self>, observer: &Arc<Observer>, mode: &str) {
        if !observer.is_valid() {
            return;
        }
        observer.note_holder(self);
        self.note_common(mode, |common| {
            if !common.observers.iter().any(|o| Arc::ptr_eq(o, observer)) {
                common.observers.push(Arc::clone(observer));
                common.observers.sort_by_key(|o| o.order());
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

    pub fn remove_observer(self: &Arc<Self>, observer: &Arc<Observer>, mode: &str) {
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

    /// [`RunLoop::remove_source_everywhere`] for an observer.
    pub fn remove_observer_everywhere(&self, observer: &Observer) {
        self.common
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .items
            .observers
            .retain(|o| !std::ptr::eq(Arc::as_ptr(o).cast::<Observer>(), observer));
        let mut modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        for entry in modes.values_mut() {
            entry
                .observers
                .retain(|o| !std::ptr::eq(Arc::as_ptr(o).cast::<Observer>(), observer));
        }
    }

    pub fn contains_observer(self: &Arc<Self>, observer: &Arc<Observer>, mode: &str) -> bool {
        // The names first, for [`RunLoop::contains_source`]'s reason.
        let names = self.names(mode);
        let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        names.iter().any(|name| {
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
    pub fn add_common_mode(self: &Arc<Self>, mode: &str) {
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

    /// The mode this loop is running in, or `None` when it is not running.
    ///
    /// `CFRunLoopCopyCurrentMode` asks exactly this, and Apple's contract is the
    /// one adopted here: a loop that is not running has no current mode, and
    /// saying so with `None` is what a caller that compares modes against a
    /// sentinel *wants* - a mode name for a loop that is not in a mode would be a
    /// lie the caller could not detect.
    ///
    /// The **innermost** run's mode, because a re-entrant
    /// `CFRunLoopRunInMode` is a different mode for as long as it lasts: a modal
    /// session that runs the loop from inside a callback of the loop that opened
    /// it is in the modal mode, and code asking what mode it is in has to be
    /// told that one. It goes back to the enclosing mode when the nested run
    /// returns, which is what a single `Option` cannot express.
    pub fn current_mode(&self) -> Option<String> {
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .map(|run| run.mode.clone())
    }

    /// Queue `run` for the next turn of the loop in `mode`.
    ///
    /// `CFRunLoopPerformBlock` returns before the block runs - that is the whole
    /// difference between it and calling the block - so the work and the mode it
    /// was asked for are recorded and nothing happens until the loop is turned.
    /// The wake-up is the other half of that promise: a loop asleep with nothing
    /// else to do has to come back to run what was just queued for it, or
    /// `CFRunLoopPerformBlock` from another thread is a message to nobody.
    pub fn perform_block(&self, mode: &str, run: impl FnOnce() + Send + 'static) {
        self.blocks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ScheduledBlock {
                mode: mode.to_string(),
                run: Box::new(run),
            });
        self.wake();
    }

    /// Whether a block queued for `scheduled` runs while the loop is in `running`.
    ///
    /// The common-modes alias cuts both ways, and both directions are load
    /// bearing: work queued under the alias must run in any mode of the set, and
    /// work queued in a member mode must run when the loop is asked to run the
    /// alias - which is what `CFRunLoopRunInMode(kCFRunLoopCommonModes, ...)`
    /// does, and a guest does it.
    fn accepts(&self, scheduled: &str, running: &str) -> bool {
        if scheduled == running {
            return true;
        }
        let members = self.names(COMMON_MODES);
        (scheduled == COMMON_MODES && members.iter().any(|name| name == running))
            || (running == COMMON_MODES && members.iter().any(|name| name == scheduled))
    }

    /// Whether `mode` has a block waiting, which is something to service.
    ///
    /// Asked by [`RunLoop::is_empty`] for the reason sources are: a loop with
    /// only queued work in it is not finished, it has not done that work yet, and
    /// answering `kCFRunLoopRunFinished` to a caller whose only reason for
    /// running the loop was to be called back is the one answer it cannot use.
    fn has_blocks(&self, mode: &str) -> bool {
        self.blocks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|block| self.accepts(&block.mode, mode))
    }

    /// Run the blocks due in `mode`, and say whether any ran.
    ///
    /// Taken out from under the lock before running, for the reason
    /// [`RunLoop::contents`] takes its sources out: a block is arbitrary work, and
    /// it is entitled to queue another one - which is what every `dispatch_async`
    /// from inside a block does.
    fn run_blocks(&self, mode: &str) -> bool {
        let mut due = Vec::new();
        {
            let mut queued = self.blocks.lock().unwrap_or_else(|e| e.into_inner());
            if queued.is_empty() {
                return false;
            }
            let mut waiting = Vec::with_capacity(queued.len());
            for block in queued.drain(..) {
                match self.accepts(&block.mode, mode) {
                    true => due.push(block),
                    false => waiting.push(block),
                }
            }
            *queued = waiting;
        }
        let ran = !due.is_empty();
        for block in due {
            (block.run)();
        }
        ran
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

    /// `CFRunLoopStop`: ask the run currently on this loop to return.
    ///
    /// The *innermost* one, and nothing at all when the loop is not running -
    /// both of which are Apple's, from `__CFRunLoopSetStopped` acting on
    /// `rl->_currentMode` and returning without a wake-up when there is no
    /// current mode. The second half is what makes a `CFRunLoopStop` issued
    /// before the loop starts a no-op rather than a stop the next run would
    /// swallow on its way in.
    pub fn stop(&self) {
        {
            let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
            match runs.last_mut() {
                Some(run) => run.stopped = true,
                None => return,
            };
        }
        self.wake();
    }

    /// Whether the innermost run on this loop has been asked to stop.
    fn is_stopped(&self) -> bool {
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .is_some_and(|run| run.stopped)
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
        if self.has_blocks(mode) {
            return false;
        }
        // The names first, for [`RunLoop::contains_source`]'s reason.
        let names = self.names(mode);
        let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        names.iter().all(|name| {
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
        // The names first, for [`RunLoop::contains_source`]'s reason.
        let names = self.names(mode);
        let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        names.iter().any(|name| {
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
        // The names first, for [`RunLoop::contains_source`]'s reason.
        let names = self.names(mode);
        let modes = self.modes.lock().unwrap_or_else(|e| e.into_inner());
        let mut sources = Vec::new();
        let mut timers = Vec::new();
        for name in names {
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
    ///
    /// The call is on the stack for as long as it lasts, so
    /// `CFRunLoopCopyCurrentMode` has an answer while it does and `None` after it
    /// - and, because it is a *stack*, a run started from inside a callback of
    /// another one gets its own entry and its own stop request rather than
    /// overwriting the enclosing run's. That is the whole difference between a
    /// re-entrant run loop working and a `CFRunLoopStop` being swallowed on the
    /// way in by the nested run that started after it.
    pub fn run_in_mode(&self, mode: &str, seconds: f64, return_after_source: bool) -> i32 {
        self.check_ownership();
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Run {
                mode: mode.to_string(),
                stopped: false,
            });
        let result = self.turn(mode, seconds, return_after_source);
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop();
        result
    }

    /// Say, once, if a thread other than this loop's owner is turning it.
    ///
    /// A diagnostic and nothing else: the answer is not changed, because the
    /// honest answer is not this runtime's to give. A run loop belongs to one
    /// thread, and two threads inside one at once is not a run loop with two
    /// stacks - it is one loop whose current mode, whose stop request and whose
    /// scheduled sources are all being driven from two places at a time.
    ///
    /// It is here because it is *silent* otherwise, and this project's rule is
    /// that a silent wrong answer is the worst outcome there is. The caller that
    /// does it is not in this crate: Chromium's message pump reaches the run loop
    /// through `-[NSRunLoop runMode:beforeDate:]`, and Foundation's version of
    /// that runs `runloop::main()` rather than the caller's own loop. Every
    /// Chromium *worker* thread therefore turns the main thread's loop, while
    /// having added its sources to its own - which is a bug in another crate,
    /// and the one sentence printed here is what names it at run time.
    ///
    /// Once per thread, so the check costs a `gettid` on a thread's first turn of
    /// any loop and nothing at all after that.
    fn check_ownership(&self) {
        thread_local! {
            static CHECKED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        if CHECKED.with(|done| done.replace(true)) {
            return;
        }
        if !self.is_own_thread() {
            let caller = darwin::pthread::id();
            crate::base::refuse_once(
                "CFRunLoopRunInMode",
                format!(
                    "thread {caller} is turning the run loop {:#p}, which belongs to thread {} - a \
                     run loop belongs to one thread, and the two will share its current mode, its \
                     stop request and its sources; the caller is \
                     `-[NSRunLoop runMode:beforeDate:]` reaching `runloop::main()` instead of \
                     `runloop::current()`",
                    self as *const RunLoop, self.owner
                ),
            );
        }
    }

    /// Whether the calling thread is the one this loop belongs to.
    ///
    /// The whole of [`RunLoop::check_ownership`]'s decision, with the printing
    /// left out so a test can ask it.
    pub fn is_own_thread(&self) -> bool {
        darwin::pthread::id() == self.owner
    }

    /// One `run_in_mode`, with the run already on the stack.
    fn turn(&self, mode: &str, seconds: f64, return_after_source: bool) -> i32 {
        self.observe(mode, activity::ENTRY);
        let deadline = match seconds.is_finite() && seconds >= 0.0 {
            true => Instant::now().checked_add(Duration::from_secs_f64(seconds.min(1e9))),
            false => None,
        };

        loop {
            if self.is_stopped() {
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
            // Everything the loop cannot see by itself is asked before anything is
            // serviced, the way a real run loop asks the kernel what is ready
            // before it dispatches: a descriptor or a Mach port that became
            // readable since the last turn is what turns a silent turn into one
            // that delivers.
            crate::socket::poll();
            #[cfg(not(target_os = "macos"))]
            crate::machport::poll();
            self.observe(mode, activity::BEFORE_TIMERS);
            for timer in &timers {
                handled |= timer.fire(now);
            }
            self.observe(mode, activity::BEFORE_SOURCES);
            for source in &sources {
                handled |= source.fire();
            }
            // After the sources and before the sleep, which is where
            // CoreFoundation runs them - so a block queued from a source's
            // `perform` runs on this turn rather than the next one.
            handled |= self.run_blocks(mode);
            if handled && return_after_source {
                self.observe(mode, activity::EXIT);
                return result::HANDLED_SOURCE;
            }
            // The stop is asked for again here, and this is the check that makes
            // it mean anything. A `CFRunLoopStop` from inside a source's `perform`
            // - which is where every Cocoa quit comes from, and where
            // `MessagePumpCFRunLoop` tears its sources down - arrives while this
            // turn is running, so the only test at the top of the loop has already
            // passed. Without this one the loop would sleep out its whole timeout
            // first, and a quit that took a second to take effect would be a
            // thread that never came back. `handled` still wins over `stopped`,
            // which is Apple's order: a run asked to return after a source was
            // handled reports that, not that it was stopped.
            if self.is_stopped() {
                self.observe(mode, activity::EXIT);
                return result::STOPPED;
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

/// A run loop of its own, not the thread's.
///
/// Only for the tests: a guest's loop is [`current`]'s, and there is exactly one
/// of those per thread.
impl Default for RunLoop {
    fn default() -> RunLoop {
        RunLoop::new()
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
        witness: 0,
        activities,
        repeats,
        order,
        callback,
        info,
        holders: Mutex::new(Vec::new()),
        valid: AtomicBool::new(true),
    });
    note(Arc::as_ptr(&observer));
    observer
}

/// Build a version-0 source with the sort key the guest asked for.
pub fn new_source_ordered(perform: Option<Perform>, info: usize, order: CFIndex) -> Arc<Source> {
    new_source_ready_ordered(perform, info, order, None)
}

/// Build a version-0 source, at order zero.
pub fn new_source(perform: Option<Perform>, info: usize) -> Arc<Source> {
    new_source_ordered(perform, info, 0)
}

/// Build a version-0 source that asks `ready` whether it has anything to do.
///
/// As [`new_source`], for a source whose work is waiting somewhere the loop
/// cannot see - see [`Source::ready`]. `ready` is handed the same `info` the
/// callback is.
pub fn new_source_ready(
    perform: Option<Perform>,
    info: usize,
    ready: Option<fn(usize) -> bool>,
) -> Arc<Source> {
    new_source_ready_ordered(perform, info, 0, ready)
}

/// [`new_source_ready`], with the guest's own `order`.
pub fn new_source_ready_ordered(
    perform: Option<Perform>,
    info: usize,
    order: CFIndex,
    ready: Option<fn(usize) -> bool>,
) -> Arc<Source> {
    let source = Arc::new(Source {
        witness: 0,
        signalled: AtomicBool::new(false),
        perform,
        info,
        order,
        holders: Mutex::new(Vec::new()),
        valid: AtomicBool::new(true),
        ready,
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
///
/// Keyed by address, and therefore only meaningful while the entry lives as long
/// as the allocation: see [`forget`], which every handle in this module calls
/// when it dies, and [`witness`], which notices when it did not.
static HANDLES: Mutex<BTreeMap<usize, TypeId>> = Mutex::new(BTreeMap::new());

/// The proof [`note`] registers for a `T`.
///
/// Every handle in this module begins with a `usize` that is written once, at
/// construction, and never again - see [`Source::witness`]. Zero is the right
/// value for it for a reason that is not taste: a real Objective-C object has its
/// `isa` in that word, and `objc::class::is_known(null)` is false, so a word that
/// is still zero cannot be an object. Neither can it be any of the other
/// non-object handles this tree hands out, because their leading words are a port
/// name, a socket descriptor and a device index.
///
/// The two questions this answers are the two that a bare address cannot, and
/// they are asked in different places: [`crate::base::is_opaque`] asks it before
/// answering for a handle at all, and [`is_noted`] asks it before rebuilding an
/// `Arc<T>` over the address.
///
/// # Safety
///
/// The caller must pass a pointer to a readable, aligned word.
unsafe fn witness<T: 'static>(handle: crate::base::CFTypeRef) -> bool {
    if handle.is_null() || (handle as usize) % std::mem::align_of::<usize>() != 0 {
        return false;
    }
    // SAFETY: the caller's obligation, as above. The word is one this module
    // wrote at construction, so reading it is reading an initialised `usize`.
    unsafe { *(handle as *const usize) == 0 }
}

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
    //
    // With the witness, which is the part that decides: this family is the one
    // whose allocations die, so it is the one the bare key cannot be trusted
    // for. See [`crate::base::OPAQUE`].
    crate::base::note_witnessed(handle.cast(), 0, Some(witness::<T>));
}

/// Record a handle the C world owns the one reference to, and how to give it
/// back.
///
/// [`note`] alone is not enough for these, and the difference is the whole of
/// what a guest-created source is. A caller that keeps the `Arc` can let it go
/// and the `Drop` below does the forgetting; `CFRunLoopSourceCreate` hands the
/// reference to the guest, and the guest gives it back only through
/// `CFRelease`. So the reference is accounted for here, and
/// [`crate::base::release_opaque`] - which `crate::api::CFRelease` reaches -
/// drops it, and the `Drop` forgets the address.
pub fn note_c_owned<T: 'static>(handle: *const T) {
    // SAFETY: `release` is only ever called with the address it was registered
    // for, and only once, because `base::release_opaque` clears the release
    // function as it fires it.
    unsafe fn release<T: 'static>(handle: crate::base::CFTypeRef) {
        // SAFETY: the C world was handed exactly this one strong reference by
        // `leak`, and `base` calls this at most once for it.
        drop(unsafe { Arc::from_raw(handle as *const T) });
    }
    crate::base::note_c_owned(handle.cast(), release::<T>);
}

/// Hand the C world a handle it owns, and record that it does.
///
/// The two halves have to be told apart deliberately. `new_source` and friends
/// hand back an `Arc` the *caller* owns, and this function is for the case where
/// ownership is being given away to the C world - so a `CFRelease` can end it.
/// Calling it without giving the reference away would let a `CFRelease` drop a
/// reference nobody handed over, which is a use-after-free reachable from a guest.
pub fn leak<T: 'static>(value: Arc<T>) -> crate::base::CFTypeRef {
    let handle = Arc::into_raw(value);
    note_c_owned(handle);
    handle.cast()
}

/// Stop answering for a handle that is dying.
///
/// The exact counterpart of [`note`], and load-bearing rather than a tidy-up.
/// A heap address identifies an allocation only until the allocation is freed,
/// and the allocator hands the same address out again — so an entry left behind
/// is a claim about whatever is built there next, and the two tables here fail in
/// opposite ways on a claim that is wrong:
///
/// - [`crate::base::is_opaque`] would answer for a *real* CoreFoundation object
///   that has taken the address of a dead handle, so `CFRetain` on it is a
///   **silent no-op** and `CFGetRetainCount` reports `1` forever. Nothing in the
///   CoreFoundation contract tells a caller to check, which is what makes this
///   worse than a crash. This is the failure that was measured, in this suite,
///   on a `__NSCFString` that had taken a dead `Observer`'s address.
/// - [`is_noted`] would answer `true` for the *wrong kind*, and
///   [`crate::api::borrow`] then rebuilds an `Arc<T>` over an allocation of a
///   different type — so a loop services a `Timer` as a `Source` and calls its
///   `info` and `perform` as the source's. That is corruption, not a wrong
///   count.
///
/// A handle reached through [`leak`] is forgotten when the guest releases it, by
/// the `Drop` this reaches through [`crate::base::release_opaque`]. A handle this
/// module hands out as an `Arc` — every source, timer and observer a caller adds
/// to a loop of its own — is forgotten when its last reference goes, which is the
/// only point at which the address means something else. And a handle a guest
/// simply never releases is a leak that stays in the table, which is a cost and
/// not a wrong answer: [`witness`] is what keeps the stale entry from claiming
/// the next object built at that address.
fn forget<T: 'static>(handle: *const T) {
    HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(handle as usize));
    crate::base::forget_opaque(handle.cast());
}

/// Whether `handle` is a `T` this module handed out, and still is one.
///
/// The second half of the question is the load-bearing half, and it is asked of
/// the *shared* table rather than of [`HANDLES`]. A `CFMachPort` and an
/// `IOHIDDeviceRef` are noted there under the same bare-address key this module
/// uses, without telling it, so a `CFMachPort` built at the address of a dead
/// `CFRunLoopSource` overwrites the shared entry while `HANDLES` still says
/// `Source` — and `crate::api::borrow::<Source>` would rebuild an `Arc<Source>`
/// over a `MachPort` and the loop would call that struct's `perform`. Asking the
/// shared table as well is what makes the two registries agree.
pub fn is_noted<T: 'static>(handle: usize) -> bool {
    HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&handle)
        == Some(&TypeId::of::<T>())
        && crate::base::witnessed(handle as *const c_void)
}

/// The four handles this module hands out stop claiming their address as it dies.
///
/// `Drop` rather than a call from `CFRelease`, because the guest has no way to
/// ask for it: it holds a `CFRunLoopSourceRef` and the only thing it can do with
/// that is `CFRelease` it, which reaches here through
/// [`crate::base::release_opaque`] and through [`leak`]. So the object has to say
/// it itself.
macro_rules! forget_on_drop {
    ($($handle:ty),* $(,)?) => {
        $(
            impl Drop for $handle {
                fn drop(&mut self) {
                    forget(self as *const $handle);
                }
            }
        )*
    };
}

forget_on_drop!(Source, Timer, Observer, RunLoop);

/// Temporary diagnostic: whether *any* kind of run-loop handle has this address.
#[cfg(test)]
pub fn probe_any_noted(handle: usize) -> bool {
    HANDLES.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&handle)
}

/// Build a timer, first due `after` from now and repeating every `interval`.
pub fn new_timer(
    after: Duration,
    interval: Option<Duration>,
    callback: Option<TimerCallback>,
    info: usize,
) -> Arc<Timer> {
    new_timer_ordered(after, interval, callback, info, 0)
}

/// [`new_timer`], with the guest's own `order`.
///
/// `CFRunLoopTimerCreate`'s order is a `CFIndex` the guest uses to sequence
/// timers within a mode, exactly as it does for sources, and the loop honours it
/// the same way.
pub fn new_timer_ordered(
    after: Duration,
    interval: Option<Duration>,
    callback: Option<TimerCallback>,
    info: usize,
    order: CFIndex,
) -> Arc<Timer> {
    let timer = Arc::new(Timer {
        witness: 0,
        next: Mutex::new(Instant::now() + after),
        interval,
        callback,
        info,
        order,
        holders: Mutex::new(Vec::new()),
        valid: AtomicBool::new(true),
        tolerance: Mutex::new(0.0),
    });
    note(Arc::as_ptr(&timer));
    timer
}

thread_local! {
    /// This thread's run loop, made on the first `CFRunLoopGetCurrent`.
    static CURRENT: std::cell::OnceCell<Arc<RunLoop>> = const { std::cell::OnceCell::new() };
}

/// The main thread's, which `CFRunLoopGetMain` answers from any thread.
///
/// Only the main thread's own loop is ever recorded here. The obvious
/// alternative - "the first thread to ask owns it" - is wrong the moment a helper
/// thread touches CoreFoundation before the main thread does, and this guest can:
/// `MessagePumpCFRunLoop`'s constructor calls `CFRunLoopGetCurrent`, and
/// Chromium builds one of those on every thread it starts, including the
/// utility and GPU children, which are `fork`ed into existence. A
/// `CFRunLoopGetMain` answered from a worker's loop is a different run loop from
/// the one AppKit is driving, and everything that then adds a source to "the main
/// run loop" adds it to a loop nothing is running.
static MAIN: Mutex<Option<Arc<RunLoop>>> = Mutex::new(None);

/// The tid of the process's **initial** thread, which is the only thread that is
/// the main one.
///
/// `std::process::id()` rather than "the first thread this runtime saw", which is
/// the obvious alternative and is the same mistake one level down: a helper
/// thread that touches CoreFoundation before the program's own `main` would
/// become the main thread, and every `CFRunLoopGetMain` after it would hand back
/// a worker's run loop. On Linux the initial thread's tid *is* the pid, so this
/// is a fact about the kernel rather than a guess about which thread arrived
/// first.
///
/// Worth knowing: `foundation`'s `+[NSThread isMainThread]` answers the same
/// question from a first-caller-wins tid, so the two can disagree on a program
/// where a thread runs before `main`. `CFRunLoopGetMain` is the one that is
/// right; the other is the approximation.
fn main_tid() -> u32 {
    std::process::id()
}

/// This thread's run loop, made if it has none.
///
/// A loop made on the main thread is published as the main run loop, so
/// `CFRunLoopGetMain` can answer it from any thread. A loop made anywhere else
/// is not: see [`MAIN`].
pub fn current() -> Arc<RunLoop> {
    CURRENT.with(|cell| {
        let loop_ = cell.get_or_init(|| {
            let loop_ = Arc::new(RunLoop::new());
            note(Arc::as_ptr(&loop_));
            loop_
        });
        if darwin::pthread::id() == main_tid() {
            let mut main = MAIN.lock().unwrap_or_else(|e| e.into_inner());
            main.get_or_insert_with(|| Arc::clone(&loop_));
        }
        Arc::clone(&loop_)
    })
}

/// The main thread's run loop, or this thread's if the main thread has not made
/// one yet.
///
/// The fallback is Apple's too, in effect: its main run loop is created before
/// the process starts, so a caller always gets *a* main run loop. Here the main
/// thread's may genuinely not exist, and answering with the caller's own is the
/// only thing that is not a lie about which thread it belongs to.
pub fn main() -> Arc<RunLoop> {
    let existing = MAIN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    existing.unwrap_or_else(current)
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

    /// A `CFRunLoopStop` asked for *before* a nested `CFRunLoopRunInMode` is still
    /// there when the nested run starts.
    ///
    /// This is the one that matters, and it is the shape a Cocoa quit takes: a
    /// source's `perform` calls `CFRunLoopStop` and then re-enters the loop -
    /// `-[NSApplication nextEventMatchingMask:...]` inside `-[NSApplication run]`
    /// is exactly that, with the stop coming from `terminate:`.
    ///
    /// With one stop flag for the whole loop, the nested run's first act is to
    /// clear it and the outer run then waits out its whole timeout: a quit that
    /// arrives at the worst moment is a program that does not quit. The known
    /// positive is the second half of the test - the *outer* run does return
    /// `STOPPED` once the nested one is out of the way - so a loop that ignored
    /// every stop request would fail here rather than pass by accident.
    #[test]
    fn a_stop_requested_before_a_nested_run_is_not_swallowed_by_it() {
        static LOOP: Mutex<Option<Arc<RunLoop>>> = Mutex::new(None);
        static NESTED: Mutex<Vec<i32>> = Mutex::new(Vec::new());
        unsafe extern "C" fn stop_then_reenter(_info: *mut c_void) {
            let borrowed = Arc::clone(LOOP.lock().unwrap_or_else(|e| e.into_inner()).as_ref().unwrap());
            // The quit, and then a nested turn - which is what a caller that has
            // to drain one more thing before it can leave does. The nested turn is
            // short, so the whole call can only be quick if the *outer* one is
            // ended by the stop rather than waiting out its own 30 seconds.
            borrowed.stop();
            NESTED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(borrowed.run_in_mode(DEFAULT_MODE, 0.05, false));
        }
        let loop_ = Arc::new(RunLoop::new());
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&loop_));
        let source = new_source(Some(stop_then_reenter), 0);
        loop_.add_source(&source, DEFAULT_MODE);
        source.signal();
        let started = Instant::now();
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 30.0, false),
            result::STOPPED,
            "the outer run has to return, and not by waiting out its timeout"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "and it has to return promptly, which is the whole claim"
        );
        // The nested run is a run like any other: it had its own stop state, so
        // it timed out rather than being ended by a request meant for the loop
        // outside it.
        assert_eq!(NESTED.lock().unwrap_or_else(|e| e.into_inner()).as_slice(), [
            result::TIMED_OUT
        ]);
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// A stop asked for *inside* a nested run ends that run and leaves the one it
    /// was nested in alone.
    ///
    /// Apple's `__CFRunLoopSetStopped` acts on `rl->_currentMode`, which is the
    /// innermost one - so this is the same rule as above seen from the other
    /// side, and getting it wrong in this direction is a modal session that
    /// cannot be dismissed.
    #[test]
    fn a_stop_inside_a_nested_run_ends_that_run_and_not_the_outer_one() {
        static LOOP: Mutex<Option<Arc<RunLoop>>> = Mutex::new(None);
        static OUTER: Mutex<Vec<i32>> = Mutex::new(Vec::new());
        unsafe extern "C" fn open_a_modal_session(_info: *mut c_void) {
            let borrowed =
                Arc::clone(LOOP.lock().unwrap_or_else(|e| e.into_inner()).as_ref().unwrap());
            // Something in the nested run dismisses it.
            let dismisser = new_source(Some(stop_now), 0);
            borrowed.add_source(&dismisser, "Modal");
            dismisser.signal();
            OUTER
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(borrowed.run_in_mode("Modal", 5.0, false));
            borrowed.remove_source(&dismisser, "Modal");
        }
        unsafe extern "C" fn stop_now(_info: *mut c_void) {
            if let Some(l) = LOOP.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                l.stop();
            }
        }
        let loop_ = Arc::new(RunLoop::new());
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&loop_));
        let source = new_source(Some(open_a_modal_session), 0);
        loop_.add_source(&source, DEFAULT_MODE);
        source.signal();
        // The outer run is only there to be *not* stopped, so it ends on its
        // deadline rather than early.
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 0.05, false),
            result::TIMED_OUT
        );
        assert_eq!(
            OUTER.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
            [result::STOPPED],
            "the nested run is the one that was stopped"
        );
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The mode a nested run is in is the one code asking inside it is told, and
    /// the enclosing mode comes back when it returns.
    ///
    /// `CFRunLoopCopyCurrentMode` from inside a `CFRunLoopRunInMode` that a
    /// callback started: a single `Option` answers the *outer* mode for the whole
    /// nested call, which is a mode name the caller is not in.
    #[test]
    fn a_nested_run_reports_its_own_mode_and_gives_the_enclosing_one_back() {
        static LOOP: Mutex<Option<Arc<RunLoop>>> = Mutex::new(None);
        static SEEN: Mutex<Vec<Option<String>>> = Mutex::new(Vec::new());
        unsafe extern "C" fn nest(_info: *mut c_void) {
            let borrowed = Arc::clone(LOOP.lock().unwrap_or_else(|e| e.into_inner()).as_ref().unwrap());
            let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
            seen.push(borrowed.current_mode());
            drop(seen);
            borrowed.add_common_mode("Modal");
            borrowed.run_in_mode("Modal", 0.0, false);
            seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
            seen.push(borrowed.current_mode());
        }
        let loop_ = Arc::new(RunLoop::new());
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&loop_));
        let source = new_source(Some(nest), 0);
        loop_.add_source(&source, DEFAULT_MODE);
        source.signal();
        loop_.run_in_mode(DEFAULT_MODE, 0.02, false);
        assert_eq!(
            SEEN.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
            [
                Some(DEFAULT_MODE.to_string()),
                // The nested run is over, so the enclosing mode is current again -
                // not "no mode", which is what a single `Option` would leave here.
                Some(DEFAULT_MODE.to_string()),
            ]
        );
        assert_eq!(loop_.current_mode(), None, "and none once the outer one is too");
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// A `CFRunLoopStop` on a loop that is not running does nothing, so the next
    /// run is a normal one.
    ///
    /// Apple's `CFRunLoopStop` reads `rl->_currentMode` first and returns
    /// without a wake-up when there is none. A stop flag the loop has to
    /// remember instead would make the *next* `CFRunLoopRunInMode` return
    /// immediately, which is the classic "quit while starting up" hang.
    #[test]
    fn a_stop_on_an_idle_loop_does_not_end_the_next_run() {
        let loop_ = Arc::new(RunLoop::new());
        let (calls, perform, _) = counter!();
        let source = new_source(Some(perform), 0);
        loop_.add_source(&source, DEFAULT_MODE);
        loop_.stop();
        source.signal();
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 1.0, true),
            result::HANDLED_SOURCE
        );
        assert_eq!(calls(), 1, "so the run really did service its source");
    }

    /// Invalidating a source takes it out of **every** mode it was added to, so a
    /// loop is not left holding a callback whose `info` the guest has freed.
    ///
    /// The guest-visible claim is `CFRunLoopContainsSource`, which is how a
    /// caller checks "is that source still in that mode". The second half is the
    /// one that matters for memory: a version-0 source's `info` is the guest's
    /// own pointer, and Apple's `CFRunLoopSourceInvalidate` removes the source
    /// "from all run loops and modes" - so after it returns, nothing this loop
    /// holds can reach the object.
    ///
    /// Both modes are covered deliberately, and the common alias is the third
    /// case: a source added under `kCFRunLoopCommonModes` is in the alias's own
    /// list *and* in every member mode, and invalidation has to reach all of them.
    #[test]
    fn invalidating_a_source_takes_it_out_of_every_mode() {
        let (calls, perform, _) = counter!();
        let loop_ = Arc::new(RunLoop::new());
        loop_.add_common_mode("Modal");
        let source = new_source(Some(perform), 0xfeed_face);
        loop_.add_source(&source, DEFAULT_MODE);
        loop_.add_source(&source, "Modal");
        assert!(loop_.contains_source(&source, DEFAULT_MODE));
        assert!(loop_.contains_source(&source, "Modal"));

        source.invalidate();
        assert!(!source.is_valid(), "and the source knows it is dead");
        for mode in [DEFAULT_MODE, "Modal", COMMON_MODES] {
            assert!(
                !loop_.contains_source(&source, mode),
                "{mode} still holds a source whose info is the guest's"
            );
        }
        // A mode with nothing left in it is finished, which is the other half of
        // what removing it buys: a loop that kept a dead source would never be
        // allowed to report that it had nothing left to do.
        assert_eq!(loop_.run_in_mode(DEFAULT_MODE, 30.0, false), result::FINISHED);
        assert_eq!(calls(), 0);
    }

    /// The same, for the alias: invalidation reaches the common set's own list and
    /// every member mode.
    ///
    /// A source added under `kCFRunLoopCommonModes` is recorded twice over - once
    /// in the alias's items and once in each mode of the set - so a removal that
    /// only walked the modes would leave the alias holding it, and the next mode
    /// made common would be handed a source that is dead and whose `info` is not
    /// the guest's any more.
    #[test]
    fn invalidating_a_source_added_under_the_alias_reaches_every_member() {
        let (calls, perform, _) = counter!();
        let loop_ = Arc::new(RunLoop::new());
        let source = new_source(Some(perform), 0);
        loop_.add_source(&source, COMMON_MODES);
        assert!(loop_.contains_source(&source, COMMON_MODES));
        source.invalidate();
        assert!(!loop_.contains_source(&source, COMMON_MODES));

        // A mode made common *afterwards* must not be handed the dead source.
        loop_.add_common_mode("Tracking");
        assert!(
            !loop_.contains_source(&source, "Tracking"),
            "a dead source is not what a new common mode inherits"
        );
        source.signal();
        assert_eq!(loop_.run_in_mode("Tracking", 0.0, true), result::FINISHED);
        assert_eq!(calls(), 0);
    }

    /// Adding a source that has already been invalidated does nothing, exactly as
    /// on a real run loop.
    ///
    /// It is the reason invalidation has to remove as well as mark: an add that
    /// took effect would put the dead source back, and a mode holding a source is
    /// a mode `CFRunLoopRunInMode` must not call finished - so the thread that
    /// asked would never come back.
    #[test]
    fn an_invalidated_source_cannot_be_added_to_a_loop() {
        let (calls, perform, _) = counter!();
        let loop_ = Arc::new(RunLoop::new());
        let source = new_source(Some(perform), 0);
        source.invalidate();
        loop_.add_source(&source, DEFAULT_MODE);
        assert!(!loop_.contains_source(&source, DEFAULT_MODE));
        source.signal();
        assert_eq!(loop_.run_in_mode(DEFAULT_MODE, 30.0, false), result::FINISHED);
        assert_eq!(calls(), 0);
    }

    /// A timer and an observer are removed on invalidation for the same reason a
    /// source is, and an invalidated one cannot be added back.
    ///
    /// One test for both because the rule is one rule: `CFRunLoopTimerInvalidate`
    /// and `CFRunLoopObserverInvalidate` "remove the timer/observer from all run
    /// loops and modes", and an observer is the one item here whose only job is
    /// to be called - so leaving a dead one installed is the surest way to call
    /// into guest memory it has freed.
    #[test]
    fn a_timer_and_an_observer_are_removed_on_invalidation_too() {
        static SEEN: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
        unsafe extern "C" fn observe(_o: CFTypeRef, _what: u32, _i: *mut c_void) {
            SEEN.lock().unwrap_or_else(|e| e.into_inner()).push("observer");
        }
        unsafe extern "C" fn tick(_t: CFTypeRef, _i: *mut c_void) {
            SEEN.lock().unwrap_or_else(|e| e.into_inner()).push("timer");
        }
        let loop_ = Arc::new(RunLoop::new());
        loop_.add_common_mode("Modal");
        let timer = new_timer_ordered(
            Duration::from_millis(1),
            Some(Duration::from_millis(1)),
            Some(tick),
            0x1234,
            0,
        );
        let observer = new_observer(activity::BEFORE_WAITING, true, 0, Some(observe), 0x5678);
        loop_.add_timer(&timer, DEFAULT_MODE);
        loop_.add_timer(&timer, "Modal");
        loop_.add_observer(&observer, DEFAULT_MODE);

        timer.invalidate();
        observer.invalidate();
        assert!(!timer.is_valid() && !observer.is_valid());
        assert!(
            !loop_.contains_observer(&observer, DEFAULT_MODE),
            "an invalidated observer is out of the mode that held it"
        );
        assert_eq!(loop_.run_in_mode(DEFAULT_MODE, 0.02, false), result::FINISHED);
        assert!(SEEN.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

        // A mode made common *after* they were invalidated must not inherit them:
        // the alias's own list is a second place they were recorded, and only a
        // removal that reaches it keeps a dead item from coming back.
        loop_.add_common_mode("Tracking");
        assert_eq!(
            loop_.run_in_mode("Tracking", 0.02, false),
            result::FINISHED,
            "a dead timer or observer is not what a new common mode inherits"
        );
        assert!(SEEN.lock().unwrap_or_else(|e| e.into_inner()).is_empty());

        // And neither can be put back, so the mode cannot be kept non-empty by a
        // dead item.
        loop_.add_timer(&timer, DEFAULT_MODE);
        loop_.add_observer(&observer, DEFAULT_MODE);
        assert!(
            !loop_.contains_observer(&observer, DEFAULT_MODE),
            "and an invalidated observer cannot be added back either"
        );
        assert_eq!(loop_.run_in_mode(DEFAULT_MODE, 0.02, false), result::FINISHED);
        assert!(SEEN.lock().unwrap_or_else(|e| e.into_inner()).is_empty());
    }

    /// Sources in one mode are serviced in the `order` the guest gave them, not
    /// in the order it happened to create them.
    ///
    /// `MessagePumpCFRunLoop` is the case in the binary: it creates its
    /// `wakeup_timer_source_` with order 1 and its `work_source_` with order 0, in
    /// that order (`Framework+0x5d95bbf` and `+0x5d95bef`), so the loop has to
    /// reverse them. Dropping the argument is not a simplification - a loop that
    /// services a different source first on every turn is a different program.
    ///
    /// The order is recorded on the source itself, so the assertion is on the
    /// recorded value as well as on the order they actually ran in: reading the
    /// run order alone would be satisfied by a sort that happened to be stable.
    #[test]
    fn sources_run_in_the_order_the_guest_gave_and_not_the_order_it_made_them() {
        static ORDER: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
        unsafe extern "C" fn late(_info: *mut c_void) {
            ORDER.lock().unwrap_or_else(|e| e.into_inner()).push("late");
        }
        unsafe extern "C" fn early(_info: *mut c_void) {
            ORDER.lock().unwrap_or_else(|e| e.into_inner()).push("early");
        }
        let loop_ = Arc::new(RunLoop::new());
        // Created in the order late-then-early, exactly as the guest does, and
        // signalled in that order too so a loop that ran them as it found them
        // would record "late", "early".
        let late_source = new_source_ordered(Some(late), 1, 1);
        let early_source = new_source_ordered(Some(early), 2, 0);
        assert_eq!(late_source.order(), 1, "the order is recorded, not guessed");
        assert_eq!(early_source.order(), 0);
        loop_.add_source(&late_source, DEFAULT_MODE);
        loop_.add_source(&early_source, DEFAULT_MODE);
        late_source.signal();
        early_source.signal();
        assert_eq!(loop_.run_in_mode(DEFAULT_MODE, 1.0, true), result::HANDLED_SOURCE);
        assert_eq!(
            ORDER.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
            ["early", "late"]
        );
    }

    /// Two sources with the same `order` keep the order they were added in, which
    /// is the only tie-break there is.
    #[test]
    fn equal_orders_keep_the_order_they_were_added_in() {
        static ORDER: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
        unsafe extern "C" fn one(_info: *mut c_void) {
            ORDER.lock().unwrap_or_else(|e| e.into_inner()).push("one");
        }
        unsafe extern "C" fn two(_info: *mut c_void) {
            ORDER.lock().unwrap_or_else(|e| e.into_inner()).push("two");
        }
        let loop_ = Arc::new(RunLoop::new());
        let first = new_source_ordered(Some(one), 0, 7);
        let second = new_source_ordered(Some(two), 0, 7);
        loop_.add_source(&first, DEFAULT_MODE);
        loop_.add_source(&second, DEFAULT_MODE);
        first.signal();
        second.signal();
        loop_.run_in_mode(DEFAULT_MODE, 1.0, true);
        assert_eq!(ORDER.lock().unwrap_or_else(|e| e.into_inner()).as_slice(), [
            "one", "two"
        ]);
    }

    /// A run loop belongs to the thread that made it, and this is how that is
    /// known.
    ///
    /// The predicate behind the one-shot diagnostic in
    /// [`RunLoop::check_ownership`], tested on its own because the diagnostic
    /// itself is a single `eprintln!` and a test cannot see one.
    ///
    /// The known positive is the first half: the thread that made the loop is
    /// told so, so a run in which every thread agreed could not pass the second
    /// half vacuously - and the guest's situation is precisely that a thread which
    /// did *not* make the loop turns one.
    #[test]
    fn a_run_loop_knows_which_thread_it_belongs_to() {
        // Register everything this thread's classes are made of *before* a
        // second thread appears. Two threads racing into class registration is
        // the documented flake in this crate (a class findable before its
        // methods are), and a test that means to spawn a thread should not be
        // the one that provokes it.
        crate::bootstrap();
        let loop_ = Arc::new(RunLoop::new());
        assert!(loop_.is_own_thread(), "the thread that made it owns it");

        let (found, listening) = std::sync::mpsc::channel::<bool>();
        let theirs = Arc::clone(&loop_);
        let worker = std::thread::spawn(move || {
            let mine = Arc::new(RunLoop::new());
            assert!(mine.is_own_thread(), "and a worker's own loop is its own");
            found
                .send(!theirs.is_own_thread())
                .expect("the test is listening");
        });
        assert!(
            listening.recv().expect("the worker made a loop"),
            "a thread that did not make a run loop is not its owner"
        );
        worker.join().expect("the worker finished");
        assert!(loop_.is_own_thread(), "and this thread still owns its own");
    }

    /// A worker's `CFRunLoopGetCurrent` does not make that worker's loop the main
    /// run loop.
    ///
    /// The obvious implementation - "the first thread to ask owns it" - is wrong
    /// the moment a helper thread touches CoreFoundation first, and this guest can
    /// do that: `MessagePumpCFRunLoop`'s constructor (`Framework+0x5d95afb`) calls
    /// `CFRunLoopGetCurrent`, and Chromium builds one on every thread it starts,
    /// utility and GPU children included. A `CFRunLoopGetMain` then answered from
    /// a worker's loop is a different loop from the one AppKit is driving, and
    /// everything that adds a source to "the main run loop" adds it to a loop
    /// nothing is running.
    ///
    /// The test is ordered so the worker's loop is made *first*, so the claim is
    /// not a race: on an implementation that publishes the first loop it sees, the
    /// assertion below sees the worker's loop and fails. It cannot be satisfied
    /// vacuously either, because the worker asserts that it really did make a loop
    /// of its own and that `main()` answered with something.
    #[test]
    fn a_worker_thread_does_not_become_the_main_run_loop() {
        // Register everything this thread's classes are made of *before* a
        // second thread appears. Two threads racing into class registration is
        // the documented flake in this crate (a class findable before its
        // methods are), and a test that means to spawn a thread should not be
        // the one that provokes it.
        crate::bootstrap();
        let (made, listening) = std::sync::mpsc::channel::<usize>();
        let worker = std::thread::spawn(move || {
            let mine = current();
            made.send(Arc::as_ptr(&mine) as usize)
                .expect("the test is listening");
            Arc::as_ptr(&mine) as usize
        });
        // The worker has now made a loop of its own. This thread is not the
        // process's main thread - the test harness did not run this on it - so
        // neither of the two may be published as the main run loop.
        listening.recv().expect("the worker made a loop");
        let mine = current();
        let worker_loop = worker.join().expect("the worker finished");
        assert_ne!(
            Arc::as_ptr(&mine) as usize, worker_loop,
            "the known positive: the two threads really do have different loops, so the \
             comparison below is not comparing a thing with itself"
        );
        assert_eq!(
            Arc::as_ptr(&main()) as usize,
            Arc::as_ptr(&mine) as usize,
            "CFRunLoopGetMain handed back a loop belonging to a thread that is not the main one \
             (the worker's is at {worker_loop:#x})"
        );
    }

    /// A loop that is not running has no current mode, and one that is running has
    /// exactly the mode it was asked for.
    ///
    /// Both halves matter, and the second is the one a caller comparing modes
    /// depends on: a modal session runs the loop in a mode of its own, and code
    /// that asks what mode it is in has to be told *that* one rather than the
    /// default it was scheduled in.
    #[test]
    fn the_current_mode_is_the_one_being_run_and_none_when_idle() {
        let loop_ = Arc::new(RunLoop::new());
        let (calls, perform, _) = counter!();
        let source = new_source(Some(perform), 0);
        loop_.add_source(&source, DEFAULT_MODE);
        assert_eq!(loop_.current_mode(), None, "not running yet");

        // Read from inside a turn, which is the only time a guest can ask: the
        // mode has to be there for the whole of the call, not just after it.
        static SEEN: Mutex<Vec<Option<String>>> = Mutex::new(Vec::new());
        unsafe extern "C" fn note(_info: *mut c_void) {
            let running = LOOP.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(loop_) = running.as_ref() {
                SEEN.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(loop_.current_mode());
            }
        }
        static LOOP: Mutex<Option<Arc<RunLoop>>> = Mutex::new(None);
        let observer_source = new_source(Some(note), 0);
        loop_.add_source(&observer_source, "Modal");
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&loop_));
        loop_.add_common_mode("Modal");

        observer_source.signal();
        assert_eq!(
            loop_.run_in_mode("Modal", 1.0, true),
            result::HANDLED_SOURCE
        );
        assert_eq!(
            SEEN.lock().unwrap_or_else(|e| e.into_inner()).as_slice(),
            [Some("Modal".to_string())],
            "a turn in a mode is in that mode"
        );
        assert_eq!(
            loop_.current_mode(),
            None,
            "and not in one after it returns"
        );
        *LOOP.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = calls;
    }

    /// A block runs on the next turn of the loop, in the mode it was scheduled
    /// for, and only once.
    ///
    /// The property a caller of `CFRunLoopPerformBlock` is buying is the delay:
    /// the call returns first, and the work happens when the loop is turned.
    #[test]
    fn a_queued_block_runs_on_the_next_turn_and_only_once() {
        static RAN: AtomicUsize = AtomicUsize::new(0);
        let loop_ = Arc::new(RunLoop::new());
        loop_.perform_block(DEFAULT_MODE, || {
            RAN.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(
            RAN.load(Ordering::SeqCst),
            0,
            "queueing it is not running it"
        );
        // A loop with only queued work in it is not finished: the work is what it
        // was run for.
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 0.0, true),
            result::HANDLED_SOURCE,
            "a block that ran is a source that was handled"
        );
        assert_eq!(RAN.load(Ordering::SeqCst), 1);
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 0.0, true),
            result::FINISHED,
            "and the block is spent, so there is nothing left to do"
        );
        assert_eq!(RAN.load(Ordering::SeqCst), 1);
    }

    /// The mode a block is queued for decides which turn runs it, and the
    /// common-modes alias works in both directions.
    #[test]
    fn a_block_runs_only_in_the_mode_it_was_queued_for() {
        static RAN: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
        let loop_ = Arc::new(RunLoop::new());
        loop_.perform_block("Modal", || RAN.lock().unwrap().push("Modal"));
        loop_.perform_block(COMMON_MODES, || RAN.lock().unwrap().push("Common"));
        loop_.perform_block(DEFAULT_MODE, || RAN.lock().unwrap().push("Default"));

        // A mode that holds none of them: nothing runs, and the blocks wait.
        assert_eq!(loop_.run_in_mode("Private", 0.0, true), result::FINISHED);
        assert!(RAN.lock().unwrap().is_empty());

        // The default mode runs the block queued for it and the one queued under
        // the alias, because the default mode is a member of the common set.
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 0.0, true),
            result::HANDLED_SOURCE
        );
        let mut ran = RAN.lock().unwrap().clone();
        ran.sort_unstable();
        assert_eq!(ran, ["Common", "Default"]);

        // And running the alias itself picks up the member-mode block, which is
        // what `CFRunLoopRunInMode(kCFRunLoopCommonModes, ...)` asks.
        loop_.perform_block(DEFAULT_MODE, || RAN.lock().unwrap().push("Default"));
        assert_eq!(
            loop_.run_in_mode(COMMON_MODES, 0.0, true),
            result::HANDLED_SOURCE
        );
        assert_eq!(RAN.lock().unwrap().len(), 3);
    }

    /// A block queued for a mode that is made common *afterwards* runs in it, for
    /// the same reason a source added under the alias does: the mode is resolved
    /// when the loop gets there, not when the work was queued.
    #[test]
    fn a_block_queued_under_the_alias_runs_in_a_mode_made_common_later() {
        static RAN: AtomicUsize = AtomicUsize::new(0);
        let loop_ = Arc::new(RunLoop::new());
        loop_.perform_block(COMMON_MODES, || {
            RAN.fetch_add(1, Ordering::SeqCst);
        });
        loop_.add_common_mode("Tracking");
        assert_eq!(
            loop_.run_in_mode("Tracking", 0.0, true),
            result::HANDLED_SOURCE
        );
        assert_eq!(RAN.load(Ordering::SeqCst), 1);
    }

    /// A block queued from inside a block runs on the *following* turn, not the
    /// one it was queued from.
    ///
    /// CoreFoundation's queue is drained into an array and then called, so work
    /// handed on from inside a block joins the next pass - and a test that says
    /// otherwise would be asserting a turn of the loop that does not exist. A
    /// block queued from a *source* is different, and does run on the same turn:
    /// sources are serviced before the block queue is reached.
    #[test]
    fn a_block_queued_from_a_block_runs_on_the_next_turn() {
        static RAN: Mutex<Vec<u32>> = Mutex::new(Vec::new());
        let loop_ = Arc::new(RunLoop::new());
        let second = Arc::clone(&loop_);
        loop_.perform_block(DEFAULT_MODE, move || {
            RAN.lock().unwrap().push(1);
            second.perform_block(DEFAULT_MODE, || {
                RAN.lock().unwrap().push(2);
            });
        });
        loop_.run_in_mode(DEFAULT_MODE, 0.0, false);
        assert_eq!(RAN.lock().unwrap().as_slice(), [1], "one block ran");
        loop_.run_in_mode(DEFAULT_MODE, 0.0, false);
        assert_eq!(
            RAN.lock().unwrap().as_slice(),
            [1, 2],
            "and the one it queued"
        );
    }

    /// A block queued from another thread wakes a loop that is asleep, which is
    /// the whole point of the call being asynchronous.
    ///
    /// A timer is what the loop is asleep *on*: a mode with nothing in it returns
    /// at once rather than waiting, so a loop waiting for a block has to be
    /// waiting for something else too.
    #[test]
    fn a_block_queued_from_another_thread_wakes_the_loop() {
        static RAN: AtomicUsize = AtomicUsize::new(0);
        let loop_ = Arc::new(RunLoop::new());
        let (_, _, callback) = counter!();
        // Far enough out that the wait is the loop's and the block's arrival is
        // what ends it.
        let timer = new_timer(Duration::from_secs(30), None, Some(callback), 0);
        loop_.add_timer(&timer, DEFAULT_MODE);
        let waker = Arc::clone(&loop_);
        let started = Instant::now();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            waker.perform_block(DEFAULT_MODE, || {
                RAN.fetch_add(1, Ordering::SeqCst);
            });
        });
        assert_eq!(
            loop_.run_in_mode(DEFAULT_MODE, 30.0, true),
            result::HANDLED_SOURCE
        );
        assert_eq!(RAN.load(Ordering::SeqCst), 1);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it slept the timeout out"
        );
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

    /// Every kind of handle stops claiming its address the moment it is destroyed.
    ///
    /// This is the whole of the fix, stated as an assertion. A heap address
    /// identifies an allocation only until the allocation is freed, and the
    /// allocator hands the same address out again - measured, not assumed: a
    /// `corefoundation` object that is a different size from a run-loop handle
    /// still lands on a dead handle's address, because a freed chunk is split
    /// rather than only reused whole. So an entry that outlives its handle is a
    /// claim about whatever is built there next, and it fails *silently*: a live
    /// object at that address answers `CFRetain` with a no-op and
    /// `CFGetRetainCount` with `1` forever, and nothing in the CoreFoundation
    /// contract tells a caller to check a count. That is a wrong answer wearing a
    /// success's clothes, which is the one outcome this project treats as worse
    /// than a crash.
    #[test]
    fn a_destroyed_handle_stops_claiming_its_address() {
        crate::bootstrap();
        let (_, perform, timer) = counter!();
        let dead: Vec<(&str, usize)> = vec![
            (
                "source",
                {
                    let source = new_source(Some(perform), 0);
                    note_address(&source)
                },
            ),
            (
                "observer",
                {
                    let observer = new_observer(activity::ENTRY, true, 0, None, 0);
                    note_address(&observer)
                },
            ),
            (
                "timer",
                {
                    let timer = new_timer(Duration::from_millis(1), None, Some(timer), 0);
                    note_address(&timer)
                },
            ),
            (
                "run loop",
                {
                    let loop_ = Arc::new(RunLoop::new());
                    note(Arc::as_ptr(&loop_));
                    note_address(&loop_)
                },
            ),
        ];
        for (kind, address) in &dead {
            // The address is a freed one, so nothing may be asked about it by
            // address any more. Reading it as a `CFRunLoopSourceRef` is what
            // `crate::api::borrow` does, and a `true` here is that call handing
            // out a `Source` built over whatever the allocator has since made.
            assert!(
                !crate::base::is_opaque(*address as *const c_void),
                "a destroyed {kind} at {address:#x} is still answered for as an opaque handle"
            );
            for other in ["Source", "Timer", "Observer", "RunLoop"] {
                assert!(
                    !is_noted_as(other, *address),
                    "a destroyed {kind} at {address:#x} still passes as a {other}, and borrow would \
                     rebuild an Arc<{other}> over whatever is there now"
                );
            }
        }
    }

    /// The kind registry is the half that corrupts rather than miscounts, and it
    /// is keyed by the same address as the opaque table.
    ///
    /// `crate::api::borrow::<T>` rebuilds an `Arc<T>` from the raw pointer the C
    /// world holds, and gates that on `is_noted::<T>`. A stale entry therefore
    /// hands a *timer* to code that believes it is holding a timer, over
    /// whatever the allocator has since built there: `CFRunLoopAddTimer` then
    /// pushes it into a mode, and the next turn of that loop reads the `next`
    /// and `callback` fields of a structure that is not a timer and calls the
    /// second of them. A class check cannot see any of this, because neither
    /// first word is an `isa` - which is why the fix is the pairing with the
    /// allocation and not a check on the object.
    ///
    /// Note what is *not* asserted: that the address is some other kind of
    /// handle. A later handle at the same address overwrites the entry with its
    /// own kind, so that half is self-consistent even when the entry is stale,
    /// and a test claiming otherwise would be asserting a bug that is not there.
    #[test]
    fn a_dead_handles_address_is_not_still_one_of_ours() {
        crate::bootstrap();
        let (_, _, callback) = counter!();
        let address = {
            let timer = new_timer(Duration::from_millis(1), None, Some(callback), 0);
            let address = note_address(&timer);
            assert!(
                is_noted::<Timer>(address),
                "a live timer is a timer, and the positive control says so"
            );
            address
        };
        for kind in ["Source", "Timer", "Observer", "RunLoop"] {
            assert!(
                !is_noted_as(kind, address),
                "the address of a destroyed timer is still handed out as a {kind} at {address:#x}, \
                 and borrow would rebuild an Arc<{kind}> over whatever the allocator has built \
                 there since"
            );
        }
    }

    /// Invalidation is not death, and forgetting at invalidation is the same bug
    /// wearing a different hat.
    ///
    /// `CFRunLoopSourceInvalidate` says "stop calling me", not "I am gone": Apple
    /// keeps the object, and a caller may still ask it for its order, its context
    /// or its type, and then release it. A registry that forgot the handle on
    /// invalidation would answer `CFGetTypeID` with 0 and send the next
    /// `CFRetain` into `objc_retain` on a structure this crate laid out - so this
    /// test pins *where* the forget belongs, and the answer is `Drop`.
    #[test]
    fn an_invalidated_handle_is_still_the_handle_it_was() {
        crate::bootstrap();
        let (_, perform, timer) = counter!();
        let source = new_source(Some(perform), 0);
        let address = note_address(&source);
        source.invalidate();
        assert!(!source.is_valid(), "invalidation does make it dead");
        assert!(
            crate::base::is_opaque(address as *const c_void),
            "an invalidated source is still a live object the C world holds"
        );
        assert!(is_noted::<Source>(address));
        // And the same for the other two, since they share the pairing.
        let made = new_timer(Duration::from_millis(1), None, Some(timer), 0);
        let at = note_address(&made);
        made.invalidate();
        assert!(crate::base::is_opaque(at as *const c_void));
        let observer = new_observer(activity::ENTRY, true, 0, None, 0);
        let ao = note_address(&observer);
        observer.invalidate();
        assert!(crate::base::is_opaque(ao as *const c_void));
    }

    /// The address of a handle that has been destroyed is the allocator's to give
    /// out again, and whatever it gives it out for must be answered as itself.
    ///
    /// The arranged condition from the guest's side, and it is worth being exact
    /// about how far the arrangement goes, because the obvious version of it
    /// proves nothing: create a source, drop it, allocate a locale, retain it.
    /// Measured, that does not collide - 0 of 768 objects over three attempts -
    /// and the reason is a size. A run-loop handle is 64 to 224 bytes and the
    /// largest CoreFoundation class in this crate is `__NSCFString` at 32, so no
    /// object can be a like-for-like replacement in the allocator's bin. The
    /// collision that actually happens in this suite is a **coalesce and a
    /// split**: a freed handle's chunk joins a neighbour's, and a smaller
    /// request is later carved out of the front of the merged region. Measured in
    /// the suite itself, under `ROSETTA3_PROBE_OPAQUE=1`, with the origin of the
    /// entry printed from `note_typed_opaque`:
    ///
    /// ```text
    /// rosetta3: probe: OPAQUE holds 0x7f6b80001400, whose first word is a class of ours
    /// rosetta3: probe: 0x7f6b80001400 was noted here:
    ///    2: corefoundation::runloop::note::<corefoundation::runloop::Observer>
    ///    3: corefoundation::runloop::new_observer
    ///    4: corefoundation::runloop::tests::observers_run_in_order
    /// ```
    ///
    /// An `Observer` a test made, dropped at the end of the test, its address
    /// handed on, and a live `__NSCFString` answering `CFGetRetainCount` with 1
    /// after a `CFRetain` that had done nothing. So this test asserts the two
    /// things that are true either way - every fresh object is a real object, and
    /// a freed address is claimed by nothing - and it is a standing guard rather
    /// than a reproduction: the deterministic witnesses that this fix is what
    /// makes true are [`a_destroyed_handle_stops_claiming_its_address`] and
    /// [`a_dead_handles_address_is_not_still_one_of_ours`].
    #[test]
    fn an_object_built_where_a_handle_used_to_be_is_a_real_object() {
        crate::bootstrap();
        // Free the addresses first, so that what follows is competing for them.
        let source = new_source(None, 0);
        let observer = new_observer(activity::ENTRY, true, 0, None, 0);
        let timer = new_timer(Duration::from_millis(1), None, None, 0);
        let freed = vec![
            handle_address(&source),
            handle_address(&observer),
            handle_address(&timer),
        ];
        // The one reference each constructor handed out goes back here, so the
        // allocations are freed and the addresses are available again.
        drop(source);
        drop(observer);
        drop(timer);
        let mut landed = 0;
        for object in 0..256 {
            let identifier = crate::string::from_bytes(format!("identifier-{object}").as_bytes(), false);
            // SAFETY: strings and objects this test makes, and it releases each.
            unsafe {
                let locale = crate::api::CFLocaleCreate(std::ptr::null(), identifier);
                let data = crate::collections::new_data(vec![0u8; object % 7]);
                let number =
                    crate::collections::new_number(crate::collections::Number::Integer(object as i64));
                for object in [locale, data, number] {
                    landed += usize::from(freed.contains(&(object as usize)));
                    assert_eq!(
                        crate::api::CFGetRetainCount(object),
                        1,
                        "a Create hands over one reference, at {object:p}"
                    );
                    crate::api::CFRetain(object);
                    assert_eq!(
                        crate::api::CFGetRetainCount(object),
                        2,
                        "CFRetain on a live object at {object:p} did nothing, so the object is \
                         being answered for by a handle that has been freed - and a retain count \
                         is not something a caller checks"
                    );
                    crate::api::CFRelease(object);
                    crate::api::CFRelease(object);
                }
                crate::api::CFRelease(identifier);
            }
        }
        eprintln!("rosetta3: note: {landed} of 768 fresh objects took a freed handle's address");
        for address in &freed {
            assert!(
                !crate::base::is_opaque(*address as *const c_void),
                "a destroyed handle still claims {address:#x}"
            );
        }
    }


    /// The word the witness reads is the first word of all four, and it is zero.
    ///
    /// The whole of [`witness`] is this field, so if a constructor ever stopped
    /// writing it - or wrote something else - every live handle in the process
    /// would answer "not a handle of mine" and `CFRetain` on one would go to
    /// Apple's `objc_retain` on a structure this crate laid out. It is a cheap
    /// invariant to pin and an expensive one to discover.
    #[test]
    fn the_witness_word_is_the_first_word_of_every_handle_and_is_zero() {
        crate::bootstrap();
        // The offsets are the load-bearing half, and they are only true because
        // of the `repr(C)` on the four structs. Measured with the default layout:
        // `Source::witness` at 32, `RunLoop::witness` at 96 - so the word the
        // witness read was a `Mutex`'s, and the check said "not a handle" about
        // every live one.
        assert_eq!(std::mem::offset_of!(Source, witness), 0, "a source's");
        assert_eq!(std::mem::offset_of!(Timer, witness), 0, "a timer's");
        assert_eq!(std::mem::offset_of!(Observer, witness), 0, "an observer's");
        assert_eq!(std::mem::offset_of!(RunLoop, witness), 0, "a run loop's");
        let (_, perform, timer) = counter!();
        let source = new_source(Some(perform), 0);
        assert_eq!(source.witness, 0, "a source's first word");
        assert_eq!(
            std::ptr::addr_of!(source.witness) as usize,
            handle_address(&source),
            "and it is the first word of the allocation, not a field the witness has to go \
             looking for"
        );
        let made = new_timer(Duration::from_millis(1), None, Some(timer), 0);
        assert_eq!(made.witness, 0, "a timer's first word");
        let observer = new_observer(activity::ENTRY, true, 0, None, 0);
        assert_eq!(observer.witness, 0, "an observer's first word");
        let loop_ = Arc::new(RunLoop::new());
        assert_eq!(loop_.witness, 0, "a run loop's first word");
        // And the witness agrees, for every kind, through the address rather than
        // through the field - which is the only way `is_opaque` can reach it.
        for (kind, address) in [
            ("Source", handle_address(&source)),
            ("Timer", handle_address(&made)),
            ("Observer", handle_address(&observer)),
            ("RunLoop", handle_address(&loop_)),
        ] {
            // SAFETY: an address of a live handle of this module, so a readable
            // aligned word is there.
            assert!(unsafe { witness::<Source>(address as *const c_void) }, "{kind}");
        }
    }

    /// A handle the **guest** owns stops claiming its address when the guest
    /// releases it.
    ///
    /// This is the half of the pairing that `Drop` alone cannot reach, and it is
    /// the half that matters: every source, timer and observer a *guest* makes
    /// arrives through `CFRunLoopSourceCreate` and friends, which hand the one
    /// strong reference to the guest, and Apple's only way for a guest to give a
    /// reference back for a run-loop handle is `CFRelease` - there is no
    /// `CFRunLoopSourceRelease`. So a registry that is paired only with `Drop` is
    /// paired with nothing at all for every handle the guest made, and the address
    /// stays claimed for as long as the process.
    ///
    /// It fails on the behaviour before the fix, and not by luck: `CFRelease` on
    /// an opaque handle was a no-op, so the entry was still there after the call
    /// that ends the handle.
    #[test]
    fn a_handle_the_guest_releases_stops_claiming_its_address() {
        crate::bootstrap();
        use crate::api::{
            CFRelease, CFRunLoopAddSource, CFRunLoopAddTimer, CFRunLoopGetCurrent,
            CFRunLoopObserverCreate, CFRunLoopSourceCreate, CFRunLoopSourceInvalidate,
            CFRunLoopTimerCreate, CFRunLoopTimerInvalidate, CFRunLoopSourceContext,
            CFRunLoopTimerContext,
        };
        let loop_ = unsafe { CFRunLoopGetCurrent() };
        let source_context = CFRunLoopSourceContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
            equal: std::ptr::null(),
            hash: std::ptr::null(),
            schedule: std::ptr::null(),
            cancel: std::ptr::null(),
            perform: None,
        };
        let timer_context = CFRunLoopTimerContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        };
        // SAFETY: three live contexts this test built, and a loop this thread owns.
        let (source, timer, observer) = unsafe {
            (
                CFRunLoopSourceCreate(std::ptr::null(), 0, &source_context),
                CFRunLoopTimerCreate(
                    std::ptr::null(),
                    crate::api::CFAbsoluteTimeGetCurrent() + 3600.0,
                    0.0,
                    0,
                    0,
                    None,
                    &timer_context as *const CFRunLoopTimerContext as *mut _,
                ),
                CFRunLoopObserverCreate(
                    std::ptr::null(),
                    activity::ENTRY,
                    1,
                    0,
                    None,
                    std::ptr::null(),
                ),
            )
        };
        for (kind, handle, is_source) in [
            ("CFRunLoopSource", source, true),
            ("CFRunLoopTimer", timer, false),
            ("CFRunLoopObserver", observer, false),
        ] {
            assert!(!handle.is_null(), "a {kind} was made");
            let address = handle as usize;
            // SAFETY: handles this test just made, on a loop this thread owns.
            unsafe {
                if is_source {
                    CFRunLoopAddSource(loop_, handle, std::ptr::null());
                    CFRunLoopSourceInvalidate(handle);
                } else if kind == "CFRunLoopTimer" {
                    CFRunLoopAddTimer(loop_, handle, std::ptr::null());
                    CFRunLoopTimerInvalidate(handle);
                } else {
                    crate::api::CFRunLoopRemoveObserver(loop_, handle, std::ptr::null());
                }
                CFRelease(handle);
            }
            assert!(
                !crate::base::is_opaque(handle),
                "a {kind} the guest released at {address:#x} is still answered for as an opaque \
                 handle, so its address is still claimed"
            );
            for other in ["Source", "Timer", "Observer", "RunLoop"] {
                assert!(
                    !is_noted_as(other, address),
                    "a released {kind} at {address:#x} still passes as a {other}"
                );
            }
        }
    }

    /// The count a guest takes on a handle it owns is a count, and it goes up.
    ///
    /// The same assertion, on the one kind of handle where it can be made
    /// deterministically. `CFGetRetainCount` used to answer a flat `1` for
    /// everything in this table, which is the answer that hid the bug: a
    /// `CFRetain` that had done nothing was indistinguishable from one that had
    /// worked. That is why the failure this whole change is about looked like a
    /// test flake and not like a wrong answer - nothing in CoreFoundation tells a
    /// caller to check a count.
    #[test]
    fn the_guests_retain_count_moves_on_a_handle_it_owns() {
        crate::bootstrap();
        use crate::api::{CFGetRetainCount, CFRetain, CFRelease, CFRunLoopSourceContext};
        let context = CFRunLoopSourceContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
            equal: std::ptr::null(),
            hash: std::ptr::null(),
            schedule: std::ptr::null(),
            cancel: std::ptr::null(),
            perform: None,
        };
        // SAFETY: a live context this test built.
        let source = unsafe { crate::api::CFRunLoopSourceCreate(std::ptr::null(), 0, &context) };
        assert!(!source.is_null());
        // SAFETY: a handle this test made, and the retain and release pair.
        unsafe {
            assert_eq!(
                CFGetRetainCount(source),
                1,
                "a Create hands over one reference"
            );
            CFRetain(source);
            assert_eq!(
                CFGetRetainCount(source),
                2,
                "CFRetain on a handle the guest owns did nothing, so the release below would be \
                 the only thing that ever ended it"
            );
            CFRelease(source);
            assert_eq!(CFGetRetainCount(source), 1, "and CFRelease gives it back");
        }
        // The handle is still live, so the last release is the one that ends it.
        // SAFETY: the last reference this test holds.
        unsafe { CFRelease(source) };
        assert!(
            !crate::base::is_opaque(source),
            "and the last one frees it, so the entry goes with the allocation - which is the whole \
             of what a bare address cannot do on its own"
        );
        assert_eq!(
            crate::base::c_retain_count(source),
            None,
            "and there is no count left, because there is no handle left"
        );
    }

    /// A release past the last one is refused, not obeyed.
    ///
    /// The handle here is still in a run loop, so the entry is still in the table,
    /// and that is what makes this case answerable at all: the count is zero and
    /// the allocation is not yet the allocator's, so `CFRelease` and `CFRetain`
    /// can be told apart from a genuine request. Both are `Spent`, and the
    /// refusal itself is a `objc::fail!` in `crate::api` - which aborts, so it is
    /// the *decision* that is pinned here and not the abort. The abort is not
    /// covered by a test, and it is worth saying so rather than letting a green
    /// run imply otherwise.
    ///
    /// The other case - a release after the entry is gone, once the `Drop` has run
    /// and the address is the allocator's - is a use-after-free here exactly as it
    /// is in Apple's CoreFoundation. See [`crate::base::release_opaque`] for why
    /// nothing here tries to catch that one, having been written and deleted.
    #[test]
    fn a_release_past_the_last_is_refused_rather_than_obeyed() {
        crate::bootstrap();
        use crate::api::{
            CFRelease, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopSourceCreate,
            CFRunLoopSourceContext,
        };
        let context = CFRunLoopSourceContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
            equal: std::ptr::null(),
            hash: std::ptr::null(),
            schedule: std::ptr::null(),
            cancel: std::ptr::null(),
            perform: None,
        };
        // SAFETY: a live context and a loop this thread owns.
        let source = unsafe {
            let source = CFRunLoopSourceCreate(std::ptr::null(), 0, &context);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, std::ptr::null());
            source
        };
        // SAFETY: the one reference the guest was handed.
        unsafe { CFRelease(source) };
        assert!(
            crate::base::is_opaque(source),
            "a loop still holds the source, so the handle is alive and the entry is right"
        );
        // SAFETY: the handle is alive: the loop is holding it. A count is a table
        // read, so asking when the answer is zero is safe - and zero is the
        // honest answer, because the C world has given its reference back and
        // the loop's own is not in this number.
        let count = unsafe { crate::api::CFGetRetainCount(source) };
        assert_eq!(
            count, 0,
            "the C world's reference is gone; the loop's is not in this number"
        );
        assert_eq!(
            crate::base::release_opaque(source),
            crate::base::CRef::Spent,
            "a second CFRelease must not reach the release function a second time"
        );
        assert_eq!(
            crate::base::retain_opaque(source),
            crate::base::CRef::Spent,
            "and neither must a CFRetain on a reference the caller does not hold"
        );
        // SAFETY: taking the source out of the loop, which is the last owner.
        unsafe { crate::api::CFRunLoopSourceInvalidate(source) };
    }

    /// A release that runs out of C references does not end a handle a run loop
    /// is still holding.
    ///
    /// The order here is the one a toolkit that never invalidates produces, and it
    /// is the reason the count and the `Arc` count are two different numbers. The
    /// guest's reference is gone; the loop's is not; so the handle is alive, every
    /// `CFRunLoopSource*` entry point must keep working on it, and the address must
    /// keep being claimed.
    #[test]
    fn a_released_handle_a_loop_still_holds_is_still_the_handle() {
        crate::bootstrap();
        use crate::api::{
            CFRelease, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopSourceCreate,
            CFRunLoopSourceContext,
        };
        let context = CFRunLoopSourceContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
            equal: std::ptr::null(),
            hash: std::ptr::null(),
            schedule: std::ptr::null(),
            cancel: std::ptr::null(),
            perform: None,
        };
        // SAFETY: a live context and a loop this thread owns.
        let (loop_, source) = unsafe {
            let source = CFRunLoopSourceCreate(std::ptr::null(), 0, &context);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, std::ptr::null());
            (CFRunLoopGetCurrent(), source)
        };
        let address = source as usize;
        // SAFETY: a handle this test made, on a loop this thread owns.
        unsafe { CFRelease(source) };
        assert!(
            is_noted::<Source>(address),
            "the loop still holds the source, so its address is still claimed and borrow must \
             still be able to find it"
        );
        // SAFETY: the source the loop is holding, which is why this is legal.
        unsafe { crate::api::CFRunLoopSourceInvalidate(source) };
        assert!(
            !is_noted::<Source>(address),
            "and invalidating took it out of the loop, which was the last reference"
        );
        // SAFETY: the loop and the source it was holding, both this test's.
        assert!(!unsafe { crate::api::CFRunLoopContainsSource(loop_, source, std::ptr::null()) } != 0);
    }

    /// An entry whose owner disclaims it is not a handle, whatever the address
    /// says.
    ///
    /// This is the state the leak used to leave behind, and it is the failure the
    /// whole change is about: a registry entry for a handle that has been
    /// destroyed, sitting on the address of a `__NSCFString` that is alive. Before
    /// the fix `is_opaque` was a lookup and the answer was `true`, so `CFRetain`
    /// on that string returned normally having done nothing and `CFGetRetainCount`
    /// answered `1` - which is what the flake in this suite was, measured.
    ///
    /// The state is constructed rather than waited for, and the reason is worth
    /// stating because "the obvious test is not a test" has already been learned
    /// here once: a run-loop handle and a `__NSCFString` are 96 and 32 bytes, so
    /// no allocation of the second can ever be a like-for-like replacement for the
    /// first, and a test that hoped for the collision would pass vacuously. (The
    /// collision that *was* observed is a coalesce-and-split - a freed chunk
    /// joined a neighbour's and a smaller request was carved out of the front - and
    /// that is not something a test can ask for on demand.) So the entry is built
    /// from the witness [`note`] registers, over a real object, and the assertions
    /// are about the policy: an owner that says "this is not mine" is believed.
    #[test]
    fn an_entry_whose_owner_disclaims_it_is_not_a_handle() {
        crate::bootstrap();
        // A real object of this runtime's, so a class check would have something
        // to fire on and there is a retain count to move.
        let object = crate::string::from_bytes(b"a string that is an object", false);
        crate::base::note_witnessed(object, 0, Some(witness::<Source>));
        assert!(
            !crate::base::is_opaque(object),
            "the address is in the table, but the owner says the allocation there is not a Source, \
             and CFRetain on a real object must not be answered for by a handle that is not there"
        );
        assert!(!crate::base::witnessed(object), "which is the same question, asked directly");
        assert_eq!(
            crate::base::opaque_type_id(object),
            None,
            "and a type read out of a disclaimed entry is a type invented for whatever the \
             allocator built there"
        );
        // The consequence, which is the only part that was ever observable.
        // SAFETY: an object this test made.
        unsafe {
            assert_eq!(crate::api::CFGetRetainCount(object), 1);
            crate::api::CFRetain(object);
            assert_eq!(
                crate::api::CFGetRetainCount(object),
                2,
                "CFRetain on a live object at {object:p} did nothing, so it is being answered for \
                 by a handle that has been freed - and a retain count is not something a caller \
                 checks"
            );
            crate::api::CFRelease(object);
        }
        crate::base::forget_opaque(object);
        // SAFETY: the one reference this test made.
        unsafe { crate::api::CFRelease(object) };
    }

    /// The address [`note`] recorded, which is the `Arc`'s data pointer.
    fn note_address<T: 'static>(handle: &Arc<T>) -> usize {
        let address = handle_address(handle);
        assert!(is_noted::<T>(address), "{address:#x} was not recorded");
        assert!(
            crate::base::is_opaque(address as *const c_void),
            "{address:#x} is not in the opaque table either"
        );
        address
    }

    /// The address of any handle, without knowing which type it is.
    fn handle_address<T: 'static>(handle: &Arc<T>) -> usize {
        std::sync::Arc::as_ptr(handle) as *const T as usize
    }

    /// Whether `address` is recorded as a handle of the named kind.
    ///
    /// By name rather than by type, because the question is "is this address some
    /// *other* kind of handle", and a test that had to name four types to ask it
    /// would be four tests.
    fn is_noted_as(kind: &str, address: usize) -> bool {
        match kind {
            "Source" => is_noted::<Source>(address),
            "Timer" => is_noted::<Timer>(address),
            "Observer" => is_noted::<Observer>(address),
            "RunLoop" => is_noted::<RunLoop>(address),
            other => unreachable!("no such kind of handle: {other}"),
        }
    }
}


