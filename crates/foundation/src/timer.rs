use std::ffi::c_void;
use std::sync::{Arc, Weak};

use corefoundation::runloop::RunLoop;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// Seconds between Apple's epoch, 2001-01-01, and the Unix one.
pub(crate) const UNIX_TO_REFERENCE: f64 = 978_307_200.0;

#[repr(C)]
pub struct Date {
    isa: *const c_void,
    /// Seconds since Apple's reference date.
    interval: f64,
}

#[repr(C)]
pub struct Timer {
    isa: *const c_void,
    /// Unretained, as Apple's is *not* - a scheduled timer retains its target.
    /// Retained here for the same reason: a target released while the timer is
    /// still scheduled would be messaged after it had gone.
    target: Id,
    user_info: Id,
    /// The run loop timer this one drives, kept as a raw `Arc` so the object can
    /// own it across the C boundary.
    timer: *const c_void,
    /// The loop the timer is scheduled on, as a raw `Weak`: a timer outliving
    /// its thread must not keep that thread's loop alive.
    run_loop: *const RunLoop,
    selector: Sel,
    interval: f64,
    fire_at: f64,
    invalidated: usize,
    repeats: usize,
    passes_timer: usize,
}

pub const DATE_INSTANCE_SIZE: usize = size_of::<Date>();
pub const TIMER_INSTANCE_SIZE: usize = size_of::<Timer>();

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
        - UNIX_TO_REFERENCE
}

/// A date object for `interval` seconds since the reference date, autoreleased.
pub fn date(interval: f64) -> Id {
    objc::arc::autorelease(make_date(interval))
}

/// A date object for `interval` seconds since the reference date, retained.
fn make_date(interval: f64) -> Id {
    let class = objc::class::lookup(c"NSDate").expect("registered by this module");
    // SAFETY: `Date` is the layout the class was registered with.
    let object = unsafe { corefoundation::base::allocate::<Date>(class) };
    // SAFETY: freshly allocated.
    unsafe { (*object).interval = interval };
    object as Id
}

/// A date object for a Unix timestamp, autoreleased.
pub fn date_since_1970(seconds: f64) -> Id {
    date(seconds - UNIX_TO_REFERENCE)
}

/// The Unix timestamp of an `NSDate`, or `None` for anything else.
///
/// Type-checked, because the caller is reading a value a guest put in a
/// dictionary: reading another class's second word as an interval is a date
/// with no relation to the one that was stored.
pub fn interval_since_1970(object: Id) -> Option<f64> {
    let class = objc::class::lookup(c"NSDate")?;
    // SAFETY: null or an object this runtime allocated.
    unsafe { corefoundation::base::is_kind_of(object as corefoundation::base::CFTypeRef, class) }
        .then(|| date_of(object) + UNIX_TO_REFERENCE)
}

fn date_of(receiver: Id) -> f64 {
    // SAFETY: a date of this module's.
    unsafe { (*(receiver as *const Date)).interval }
}

unsafe extern "C" fn date_now(_class: Id, _sel: Sel) -> Id {
    date(now())
}

unsafe extern "C" fn distant_future(_class: Id, _sel: Sel) -> Id {
    // Apple's is about ten thousand years out. The exact value is not the
    // interface; being past every deadline a program computes is.
    date(now() + 3600.0 * 24.0 * 365.0 * 10_000.0)
}

unsafe extern "C" fn distant_past(_class: Id, _sel: Sel) -> Id {
    date(now() - 3600.0 * 24.0 * 365.0 * 10_000.0)
}

unsafe extern "C" fn date_with_interval_since_now(_class: Id, _sel: Sel, seconds: f64) -> Id {
    date(now() + seconds)
}

/// `+dateWithTimeIntervalSince1970:`, which is the one a program building a
/// date out of a server timestamp uses.
unsafe extern "C" fn date_with_interval_since_1970(_class: Id, _sel: Sel, seconds: f64) -> Id {
    date_since_1970(seconds)
}

/// `+dateWithTimeIntervalSinceReferenceDate:`, the same constructor in Apple's
/// own epoch.
unsafe extern "C" fn date_with_interval_since_reference(_class: Id, _sel: Sel, seconds: f64) -> Id {
    date(seconds)
}

/// `-dateByAddingTimeInterval:`, the one arithmetic method on a date.
///
/// A new object rather than a mutation: `NSDate` is immutable, and a caller
/// that got its own receiver back with a moved interval would find every date
/// it had kept a reference to had moved with it.
unsafe extern "C" fn date_by_adding_interval(receiver: Id, _sel: Sel, seconds: f64) -> Id {
    date(date_of(receiver) + seconds)
}

/// `-compare:`, which answers `NSComparisonResult`: -1, 0 or 1, in that
/// numbering and no other. A caller sorts with it and branches on the sign.
///
/// Two dates that are equal to the microsecond are equal here, because a date
/// *is* its interval - there is nothing else to break a tie with.
unsafe extern "C" fn compare(receiver: Id, _sel: Sel, other: Id) -> isize {
    let (mine, theirs) = (date_of(receiver), date_of(other));
    match mine.partial_cmp(&theirs) {
        Some(std::cmp::Ordering::Less) => -1,
        Some(std::cmp::Ordering::Greater) => 1,
        // Equal, and also the answer for a NaN interval: `NSOrderedSame` is the
        // only one of the three that does not claim an order that is not there.
        _ => 0,
    }
}

unsafe extern "C" fn is_equal_to_date(receiver: Id, _sel: Sel, other: Id) -> u8 {
    u8::from(date_of(receiver) == date_of(other))
}

/// `-earlierDate:` and `-laterDate:`, which answer with *one of the two
/// receivers* rather than a new date - a caller compares the answer against its
/// own pointer.
unsafe extern "C" fn earlier_date(receiver: Id, _sel: Sel, other: Id) -> Id {
    match date_of(receiver) <= date_of(other) {
        true => receiver,
        false => other,
    }
}

unsafe extern "C" fn later_date(receiver: Id, _sel: Sel, other: Id) -> Id {
    match date_of(receiver) >= date_of(other) {
        true => receiver,
        false => other,
    }
}

/// `+[NSDate timeIntervalSinceReferenceDate]`: the current time, no object.
unsafe extern "C" fn class_time_interval_since_reference(_class: Id, _sel: Sel) -> f64 {
    now()
}

unsafe extern "C" fn time_interval_since_reference(receiver: Id, _sel: Sel) -> f64 {
    date_of(receiver)
}

unsafe extern "C" fn time_interval_since_now(receiver: Id, _sel: Sel) -> f64 {
    date_of(receiver) - now()
}

unsafe extern "C" fn time_interval_since_date(receiver: Id, _sel: Sel, other: Id) -> f64 {
    date_of(receiver) - date_of(other)
}

unsafe extern "C" fn time_interval_since_1970(receiver: Id, _sel: Sel) -> f64 {
    date_of(receiver) + UNIX_TO_REFERENCE
}

fn timer_of<'a>(receiver: Id) -> &'a mut Timer {
    // SAFETY: a timer of this module's.
    unsafe { &mut *(receiver as *mut Timer) }
}

/// Every scheduled timer, so the run loop's `info` word can find its object.
///
/// The `info` is the timer object's address, and the entry keeps the object
/// alive: a program is entitled to schedule a timer and drop its own reference,
/// which is why `-invalidate` is the documented way to stop one.
static SCHEDULED: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

/// One turn of a timer: send the selector to the target.
unsafe extern "C" fn fire(native_timer: *const c_void, info: *mut c_void) {
    let object = info as Id;
    let (target, selector, repeats, user_info, passes_timer) = {
        let timer = timer_of(object);
        if timer.invalidated != 0 {
            return;
        }
        if !native_timer.is_null() && timer.repeats != 0 {
            timer.fire_at = now() + timer.interval;
        }
        (
            timer.target,
            timer.selector,
            timer.repeats,
            timer.user_info,
            timer.passes_timer,
        )
    };
    if target.is_null() || selector.is_null() {
        return;
    }
    // SAFETY: a live object this runtime allocated.
    let method = unsafe { objc::object::class_of(target) }.and_then(|c| c.method(selector));
    let Some(method) = method else {
        // A target that does not answer is the program's error, and Apple
        // raises. Named rather than silent: a timer that fires into nothing is
        // a frame loop that does not run, which is what this whole module
        // exists to stop happening quietly.
        objc::fail!(
            "-[NSTimer fire]: the target does not respond to the selector it was scheduled with"
        );
    };
    // SAFETY: a timer action takes the timer, while delayed performing takes
    // the object supplied by its caller. Both are `void (id, SEL, id)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(method.implementation());
        f(
            target,
            selector,
            if passes_timer != 0 { object } else { user_info },
        );
    }
    if repeats == 0 {
        // SAFETY: `-invalidate` is `void (id, SEL)`.
        unsafe { invalidate(object, objc::sel::get("invalidate")) };
    }
}

/// Build a timer object, scheduling it on the calling thread's loop if asked,
/// which is where Apple's `+scheduledTimer...` and delayed performs go.
fn make(
    interval: f64,
    target: Id,
    selector: Sel,
    user_info: Id,
    repeats: bool,
    schedule: bool,
    passes_timer: bool,
) -> Id {
    let class = objc::class::lookup(c"NSTimer").expect("registered by this module");
    // SAFETY: `Timer` is the layout the class was registered with.
    let object = unsafe { corefoundation::base::allocate::<Timer>(class) };
    // A zero or negative interval is Apple's "as soon as possible", and a run
    // loop timer with one would fire in a tight loop; a tenth of a millisecond
    // is the smallest this tree will ask the loop for.
    let interval = interval.max(0.0001);
    // SAFETY: freshly allocated; every field is written before it is read.
    unsafe {
        (*object).target = objc::arc::retain(target);
        (*object).user_info = objc::arc::retain(user_info);
        (*object).selector = selector;
        (*object).interval = interval;
        (*object).fire_at = now() + interval;
        (*object).invalidated = 0;
        (*object).repeats = repeats as usize;
        (*object).passes_timer = passes_timer as usize;
        (*object).timer = std::ptr::null();
        (*object).run_loop = std::ptr::null();
    }
    let object = object as Id;
    if schedule {
        schedule_on(object, &corefoundation::runloop::current());
    }
    objc::arc::autorelease(object)
}

/// Schedule an already-built timer on `run_loop`.
fn schedule_on(object: Id, run_loop: &Arc<RunLoop>) {
    schedule_in(
        object,
        run_loop,
        &[corefoundation::runloop::DEFAULT_MODE.to_string()],
    );
}

/// Schedule an already-built timer on `run_loop` in each of `modes`.
///
/// A timer already on that loop joins the further modes; one on another loop
/// stays where it is, since a timer belongs to one loop only.
fn schedule_in(object: Id, run_loop: &Arc<RunLoop>, modes: &[String]) {
    let timer = timer_of(object);
    if timer.invalidated != 0 {
        return;
    }
    if !timer.timer.is_null() {
        if timer.run_loop.is_null() {
            return;
        }
        // SAFETY: the `Weak` `schedule_in` leaked into the object, borrowed.
        let scheduled = std::mem::ManuallyDrop::new(unsafe { Weak::from_raw(timer.run_loop) });
        if scheduled
            .upgrade()
            .is_some_and(|l| Arc::ptr_eq(&l, run_loop))
        {
            // SAFETY: the `Arc` this module leaked into the object, borrowed.
            let run_loop_timer = std::mem::ManuallyDrop::new(unsafe {
                Arc::from_raw(timer.timer as *const corefoundation::runloop::Timer)
            });
            for mode in modes {
                run_loop.add_timer(&run_loop_timer, mode);
            }
        }
        return;
    }
    let interval = std::time::Duration::from_secs_f64(timer.interval);
    let repeat = (timer.repeats != 0).then_some(interval);
    let delay = std::time::Duration::from_secs_f64((timer.fire_at - now()).max(0.0));
    let run_loop_timer =
        corefoundation::runloop::new_timer(delay, repeat, Some(fire), object as usize);
    for mode in modes {
        run_loop.add_timer(&run_loop_timer, mode);
    }
    // Kept alive by the object, and by the table: a scheduled timer outlives
    // the reference the program holds.
    objc::arc::retain(object);
    SCHEDULED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(object as usize);
    timer.timer = Arc::into_raw(run_loop_timer) as *const c_void;
    timer.run_loop = Weak::into_raw(Arc::downgrade(run_loop));
}

unsafe extern "C" fn scheduled_timer(
    _class: Id,
    _sel: Sel,
    interval: f64,
    target: Id,
    selector: Sel,
    user_info: Id,
    repeats: u8,
) -> Id {
    make(
        interval,
        target,
        selector,
        user_info,
        repeats != 0,
        true,
        true,
    )
}

unsafe extern "C" fn unscheduled_timer(
    _class: Id,
    _sel: Sel,
    interval: f64,
    target: Id,
    selector: Sel,
    user_info: Id,
    repeats: u8,
) -> Id {
    make(
        interval,
        target,
        selector,
        user_info,
        repeats != 0,
        false,
        true,
    )
}

unsafe extern "C" fn perform_after_delay(
    receiver: Id,
    _sel: Sel,
    selector: Sel,
    object: Id,
    delay: f64,
) {
    let _ = make(delay, receiver, selector, object, false, true, false);
}

/// Invalidate every pending `-performSelector:withObject:afterDelay:` on
/// `target` that `matches` accepts.
fn cancel_delayed_performs(target: Id, matches: impl Fn(&Timer) -> bool) {
    // Retained and matched outside the lock: `matches` may send `-isEqual:`,
    // and `-invalidate` takes the lock itself.
    let pending: Vec<Id> = SCHEDULED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|&object| object as Id)
        .filter(|&object| {
            let timer = timer_of(object);
            timer.passes_timer == 0 && timer.target == target
        })
        .map(objc::arc::retain)
        .collect();
    for object in pending {
        if matches(timer_of(object)) {
            // SAFETY: `-invalidate` is `void (id, SEL)`; a no-op on a timer
            // that fired or was cancelled since the table was read.
            unsafe { invalidate(object, objc::sel::get("invalidate")) };
        }
        objc::arc::release(object);
    }
}

unsafe extern "C" fn cancel_previous_performs(_class: Id, _sel: Sel, target: Id) {
    cancel_delayed_performs(target, |_| true);
}

/// The argument is matched with `-isEqual:`, as Apple documents, so a nil
/// argument matches only requests made with nil.
unsafe extern "C" fn cancel_previous_performs_matching(
    _class: Id,
    _sel: Sel,
    target: Id,
    selector: Sel,
    argument: Id,
) {
    cancel_delayed_performs(target, |timer| {
        timer.selector == selector
            && (timer.user_info == argument
                || (!argument.is_null()
                    && !timer.user_info.is_null()
                    // SAFETY: `-isEqual:` is `BOOL (id, SEL, id)`, sent to a
                    // live argument the request retained.
                    && unsafe {
                        let f: unsafe extern "C" fn(Id, Sel, Id) -> u8 =
                            std::mem::transmute(objc::msg::entry());
                        f(argument, objc::sel::get("isEqual:"), timer.user_info) != 0
                    }))
    });
}

/// The state owned by a `-performSelectorOnMainThread:...` queue entry.
///
/// Both objects are retained before the entry is submitted. This is necessary
/// even for the synchronous case: another thread can release either object
/// while the caller is waiting for the main thread to run it.
struct MainThreadPerform {
    receiver: Id,
    selector: Sel,
    object: Id,
}

/// Invoke one main-thread selector and release the queue entry's ownership.
///
/// # Safety
///
/// `context` must be a `Box<MainThreadPerform>` made by
/// [`perform_on_main_thread`] and consumed exactly once.
unsafe extern "C" fn run_main_thread_perform(context: *mut c_void) {
    let perform = unsafe { Box::from_raw(context.cast::<MainThreadPerform>()) };
    // Resolve here, on the main thread. Besides matching normal message
    // delivery, this lets a method installed after submission be the one run.
    let Some(method) = (unsafe { objc::object::class_of(perform.receiver) })
        .and_then(|class| class.method(perform.selector))
    else {
        objc::fail!(
            "-performSelectorOnMainThread:withObject:waitUntilDone: target does not respond to selector"
        );
    };
    // SAFETY: this API requires a selector whose method takes one object.
    let function: unsafe extern "C" fn(Id, Sel, Id) =
        unsafe { std::mem::transmute(method.implementation()) };
    // SAFETY: the retained receiver and argument are live for this call.
    unsafe { function(perform.receiver, perform.selector, perform.object) };
    objc::arc::release(perform.object);
    objc::arc::release(perform.receiver);
}

unsafe extern "C" fn perform_on_main_thread(
    receiver: Id,
    _sel: Sel,
    selector: Sel,
    object: Id,
    wait: i8,
) {
    let perform = Box::new(MainThreadPerform {
        receiver: objc::arc::retain(receiver),
        selector,
        object: objc::arc::retain(object),
    });
    let context = Box::into_raw(perform) as u64;
    let function = run_main_thread_perform as *const () as u64;
    let main = darwin::dispatch::queue::main_queue();
    // SAFETY: the boxed context is consumed exactly once by the function. A
    // synchronous submission returns only after that has happened; an
    // asynchronous one transfers ownership to the queue.
    unsafe {
        if wait != 0 {
            darwin::dispatch::queue::sync_f(main, context, function);
        } else {
            darwin::dispatch::queue::async_f(main, context, function);
        }
    }
}

unsafe extern "C" fn invalidate(receiver: Id, _sel: Sel) {
    let timer = timer_of(receiver);
    timer.invalidated = 1;
    if timer.timer.is_null() {
        return;
    }
    // SAFETY: an `Arc` this module leaked into the object; taken back exactly
    // once, because the pointer is cleared below.
    let run_loop_timer =
        unsafe { Arc::from_raw(timer.timer as *const corefoundation::runloop::Timer) };
    timer.timer = std::ptr::null();
    if !timer.run_loop.is_null() {
        // SAFETY: the `Weak` `schedule_in` leaked into the object, taken back
        // once with the timer above.
        let run_loop = unsafe { Weak::from_raw(timer.run_loop) };
        timer.run_loop = std::ptr::null();
        if let Some(run_loop) = run_loop.upgrade() {
            run_loop.remove_timer_everywhere(&run_loop_timer);
        }
    }
    run_loop_timer.invalidate();
    SCHEDULED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|t| *t != receiver as usize);
    objc::arc::release(receiver);
}

unsafe extern "C" fn is_valid(receiver: Id, _sel: Sel) -> u8 {
    (timer_of(receiver).invalidated == 0) as u8
}

unsafe extern "C" fn timer_fire(receiver: Id, _sel: Sel) {
    // SAFETY: the run loop's own callback, with this object as its `info`.
    unsafe { fire(std::ptr::null(), receiver as *mut c_void) };
}

unsafe extern "C" fn user_info(receiver: Id, _sel: Sel) -> Id {
    timer_of(receiver).user_info
}

unsafe extern "C" fn time_interval(receiver: Id, _sel: Sel) -> f64 {
    timer_of(receiver).interval
}

unsafe extern "C" fn fire_date(receiver: Id, _sel: Sel) -> Id {
    date(timer_of(receiver).fire_at)
}

unsafe extern "C" fn set_fire_date(receiver: Id, _sel: Sel, date: Id) {
    let timer = timer_of(receiver);
    if date.is_null() || timer.invalidated != 0 {
        return;
    }
    let value = date_of(date);
    if !value.is_finite() {
        return;
    }
    let Ok(delay) = std::time::Duration::try_from_secs_f64((value - now()).max(0.0)) else {
        return;
    };
    let Some(when) = std::time::Instant::now().checked_add(delay) else {
        return;
    };
    timer.fire_at = value;
    if !timer.timer.is_null() {
        // SAFETY: this timer owns the Arc; borrow it without consuming ownership.
        let native = unsafe { &*(timer.timer as *const corefoundation::runloop::Timer) };
        native.set_next(when);
        if !timer.run_loop.is_null() {
            // SAFETY: the Weak stored by schedule_in, borrowed without consuming it.
            let loop_ = std::mem::ManuallyDrop::new(unsafe { Weak::from_raw(timer.run_loop) });
            if let Some(loop_) = loop_.upgrade() {
                loop_.wake();
            }
        }
    }
}

/// An `NSRunLoop`: the Core Foundation loop it fronts.
///
/// Held weakly: the loop belongs to its thread, and an object a program kept
/// after the thread ended must not keep the loop alive with it.
#[repr(C)]
struct RunLoopObject {
    isa: *const c_void,
    run_loop: *const RunLoop,
}

/// An object for `run_loop`, never released: one per thread that asks, and a
/// release from a thread-local destructor could run after the runtime's own
/// thread-locals are gone.
fn new_run_loop_object(run_loop: &Arc<RunLoop>) -> Id {
    let class = objc::class::lookup(c"NSRunLoop").expect("registered by this module");
    // SAFETY: `RunLoopObject` is the layout the class was registered with.
    let object = unsafe { corefoundation::base::allocate::<RunLoopObject>(class) };
    // SAFETY: freshly allocated.
    unsafe { (*object).run_loop = Weak::into_raw(Arc::downgrade(run_loop)) };
    object as Id
}

/// `+mainRunLoop`, the same object from every thread.
fn main_run_loop_object() -> Id {
    static MAIN: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *MAIN.get_or_init(|| new_run_loop_object(&corefoundation::runloop::main()) as usize) as Id
}

thread_local! {
    static THREAD_RUN_LOOP: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `+currentRunLoop`: the calling thread's own loop, as `CFRunLoopGetCurrent`
/// answers it. A secondary thread that ran the main loop here would service
/// the main thread's sources - window drawing among them - beside it.
fn current_run_loop_object() -> Id {
    // SAFETY: this asks about the calling thread.
    if unsafe { darwin::pthread::main_np() } != 0 {
        return main_run_loop_object();
    }
    THREAD_RUN_LOOP.with(|object| {
        if object.get() == 0 {
            object.set(new_run_loop_object(&corefoundation::runloop::current()) as usize);
        }
        object.get() as Id
    })
}

/// The loop an `NSRunLoop` fronts, while its thread still has it.
pub(crate) fn run_loop_of(receiver: Id) -> Option<Arc<RunLoop>> {
    if receiver.is_null() {
        return None;
    }
    // SAFETY: an `NSRunLoop` of this module's.
    let weak = unsafe { (*(receiver as *const RunLoopObject)).run_loop };
    if weak.is_null() {
        return None;
    }
    // SAFETY: the `Weak` the object owns, borrowed rather than taken.
    std::mem::ManuallyDrop::new(unsafe { Weak::from_raw(weak) }).upgrade()
}

unsafe extern "C" fn current_run_loop(_class: Id, _sel: Sel) -> Id {
    current_run_loop_object()
}

unsafe extern "C" fn main_run_loop(_class: Id, _sel: Sel) -> Id {
    main_run_loop_object()
}

/// A mode argument as a name: nil is the default mode.
pub(crate) fn mode_name(mode: Id) -> String {
    match mode.is_null() {
        true => corefoundation::runloop::DEFAULT_MODE.to_string(),
        // SAFETY: an `NSString`, as the methods taking a mode declare.
        false => String::from_utf8_lossy(&unsafe { crate::string::bytes(mode) }).into_owned(),
    }
}

/// `-addTimer:forMode:`, in that mode: a program adds a timer for the common
/// modes or a tracking mode precisely so it fires while the default mode is
/// not the one running.
unsafe extern "C" fn add_timer_for_mode(receiver: Id, _sel: Sel, timer: Id, mode: Id) {
    if let Some(run_loop) = run_loop_of(receiver)
        && !timer.is_null()
    {
        schedule_in(timer, &run_loop, &[mode_name(mode)]);
    }
}

/// `-getCFRunLoop`, borrowed as `CFRunLoopGetCurrent`'s answer is.
unsafe extern "C" fn get_cf_run_loop(receiver: Id, _sel: Sel) -> Id {
    run_loop_of(receiver).map_or(std::ptr::null_mut(), |run_loop| {
        Arc::as_ptr(&run_loop) as Id
    })
}

/// `-currentMode`: nil when the loop is not running.
unsafe extern "C" fn current_mode(receiver: Id, _sel: Sel) -> Id {
    let Some(run_loop) = run_loop_of(receiver) else {
        return std::ptr::null_mut();
    };
    // SAFETY: a live loop, kept so by `run_loop` for the call.
    let mode = unsafe {
        corefoundation::api::CFRunLoopCopyCurrentMode(
            Arc::as_ptr(&run_loop) as corefoundation::base::CFTypeRef
        )
    };
    if mode.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a fresh string this call owns.
    unsafe { corefoundation::api::CFAutorelease(mode) as Id }
}

/// `-runMode:beforeDate:`, which is `CFRunLoopRunInMode` on the receiver's loop.
unsafe extern "C" fn run_mode_before_date(receiver: Id, _sel: Sel, mode: Id, limit: Id) -> u8 {
    let Some(run_loop) = run_loop_of(receiver) else {
        return 0;
    };
    let seconds = match limit.is_null() {
        true => 0.0,
        false => (date_of(limit) - now()).max(0.0),
    };
    let result = run_loop.run_in_mode(&mode_name(mode), seconds, true);
    (result != corefoundation::runloop::result::FINISHED) as u8
}

unsafe extern "C" fn run_until_date(receiver: Id, sel: Sel, limit: Id) {
    // SAFETY: `-runMode:beforeDate:` is `BOOL (id, SEL, id, id)`.
    unsafe { run_mode_before_date(receiver, sel, std::ptr::null_mut(), limit) };
}

pub(crate) fn install() {
    let nsobject = objc::class::lookup(c"NSObject").expect("the runtime's own NSObject");
    methods!(nsobject, {
        "performSelector:withObject:afterDelay:" => perform_after_delay,
            c"v40@0:8:16@24d32";
        "performSelector:withObject:afterDelay:inModes:" => perform_after_delay_in_modes,
            c"v48@0:8:16@24d32@40";
        "performSelectorOnMainThread:withObject:waitUntilDone:" => perform_on_main_thread,
            c"v40@0:8:16@24c32";
    });
    class_methods!(nsobject, {
        "cancelPreviousPerformRequestsWithTarget:" => cancel_previous_performs, c"v24@0:8@16";
        "cancelPreviousPerformRequestsWithTarget:selector:object:"
            => cancel_previous_performs_matching, c"v40@0:8@16:24@32";
    });

    let date_class = corefoundation::base::register(c"NSDate", None, DATE_INSTANCE_SIZE, false);
    corefoundation::base::note_type_class(date_class);
    class_methods!(date_class, {
        "date" => date_now, c"@16@0:8";
        "distantFuture" => distant_future, c"@16@0:8";
        "distantPast" => distant_past, c"@16@0:8";
        "dateWithTimeIntervalSinceNow:" => date_with_interval_since_now, c"@24@0:8d16";
        "dateWithTimeIntervalSince1970:" => date_with_interval_since_1970, c"@24@0:8d16";
        "dateWithTimeIntervalSinceReferenceDate:" => date_with_interval_since_reference,
            c"@24@0:8d16";
        "timeIntervalSinceReferenceDate" => class_time_interval_since_reference, c"d16@0:8";
    });
    methods!(date_class, {
        "timeIntervalSinceReferenceDate" => time_interval_since_reference, c"d16@0:8";
        "timeIntervalSinceNow" => time_interval_since_now, c"d16@0:8";
        "timeIntervalSince1970" => time_interval_since_1970, c"d16@0:8";
        "timeIntervalSinceDate:" => time_interval_since_date, c"d24@0:8@16";
        "dateByAddingTimeInterval:" => date_by_adding_interval, c"@24@0:8d16";
        "compare:" => compare, c"q24@0:8@16";
        "isEqualToDate:" => is_equal_to_date, c"c24@0:8@16";
        "earlierDate:" => earlier_date, c"@24@0:8@16";
        "laterDate:" => later_date, c"@24@0:8@16";
    });

    let timer_class = corefoundation::base::register(c"NSTimer", None, TIMER_INSTANCE_SIZE, false);
    class_methods!(timer_class, {
        "scheduledTimerWithTimeInterval:target:selector:userInfo:repeats:"
            => scheduled_timer, c"@48@0:8d16@24:32@40c48";
        "timerWithTimeInterval:target:selector:userInfo:repeats:"
            => unscheduled_timer, c"@48@0:8d16@24:32@40c48";
    });
    methods!(timer_class, {
        "invalidate" => invalidate, c"v16@0:8";
        "isValid" => is_valid, c"B16@0:8";
        "fire" => timer_fire, c"v16@0:8";
        "userInfo" => user_info, c"@16@0:8";
        "timeInterval" => time_interval, c"d16@0:8";
        "fireDate" => fire_date, c"@16@0:8";
        "setFireDate:" => set_fire_date, c"v24@0:8@16";
    });

    let run_loop =
        corefoundation::base::register(c"NSRunLoop", None, size_of::<RunLoopObject>(), false);
    class_methods!(run_loop, {
        "currentRunLoop" => current_run_loop, c"@16@0:8";
        "mainRunLoop" => main_run_loop, c"@16@0:8";
    });
    methods!(run_loop, {
        "addTimer:forMode:" => add_timer_for_mode, c"v32@0:8@16@24";
        "runMode:beforeDate:" => run_mode_before_date, c"B32@0:8@16@24";
        "runUntilDate:" => run_until_date, c"v24@0:8@16";
        "getCFRunLoop" => get_cf_run_loop, c"^v16@0:8";
        "currentMode" => current_mode, c"@16@0:8";
    });
}

/// Core Foundation's toll-free bridge to `NSDate`.
pub mod cf {
    #![allow(non_snake_case)]

    use corefoundation::base::{CFTypeID, CFTypeRef};
    use objc::object::Id;

    unsafe extern "C" fn CFDateGetTypeID() -> CFTypeID {
        let class = objc::class::lookup(c"NSDate").expect("registered by timer::install");
        corefoundation::base::type_id(class)
    }

    unsafe extern "C" fn CFDateCreate(_allocator: CFTypeRef, interval: f64) -> Id {
        super::make_date(interval)
    }

    unsafe extern "C" fn CFDateGetAbsoluteTime(date: Id) -> f64 {
        super::date_of(date)
    }

    exports::exports! {
        functions {
            "CFDateGetTypeID" => CFDateGetTypeID,
            "CFDateCreate" => CFDateCreate,
            "CFDateGetAbsoluteTime" => CFDateGetAbsoluteTime,
        }
    }
}

unsafe extern "C" fn perform_after_delay_in_modes(
    receiver: Id,
    _sel: Sel,
    selector: Sel,
    object: Id,
    delay: f64,
    modes: Id,
) {
    // SAFETY: an array of strings, or nil.
    let modes: Vec<String> =
        unsafe { corefoundation::collections::array(modes as corefoundation::base::CFTypeRef) }
            .map(|array| {
                array
                    .values
                    .iter()
                    .map(|mode| {
                        String::from_utf8_lossy(&unsafe { crate::string::bytes(*mode as Id) })
                            .into_owned()
                    })
                    .collect()
            })
            .unwrap_or_default();
    if modes.is_empty() {
        return;
    }
    let timer = make(delay, receiver, selector, object, false, false, false);
    schedule_in(timer, &corefoundation::runloop::current(), &modes);
}

#[cfg(test)]
mod tests {
    fn pending_perform_count(target: Id) -> usize {
        SCHEDULED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|&&object| {
                let timer = timer_of(object as Id);
                timer.passes_timer == 0 && timer.target == target
            })
            .count()
    }

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static REPEATED: AtomicUsize = AtomicUsize::new(0);
    static ONCE: AtomicUsize = AtomicUsize::new(0);
    static MAIN_THREAD_ARGUMENT: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn repeated(_receiver: Id, _sel: Sel, _timer: Id) {
        REPEATED.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn once(_receiver: Id, _sel: Sel, _timer: Id) {
        ONCE.fetch_add(1, Ordering::SeqCst);
    }

    static CANCEL_ARGUMENTS: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());
    static CANCEL_ALL: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn record_argument(_receiver: Id, _sel: Sel, object: Id) {
        CANCEL_ARGUMENTS.lock().unwrap().push(object as usize);
    }

    unsafe extern "C" fn count_cancel_all(_receiver: Id, _sel: Sel, _object: Id) {
        CANCEL_ALL.fetch_add(1, Ordering::SeqCst);
    }

    unsafe extern "C" fn performed_on_main(_receiver: Id, _sel: Sel, object: Id) {
        MAIN_THREAD_ARGUMENT.store(object as usize, Ordering::SeqCst);
    }

    /// A target of this test's own.
    ///
    /// Two, one per test, because the tests share a process: a shared counter
    /// would count another test's firings too.
    fn target(name: &std::ffi::CStr, imp: unsafe extern "C" fn(Id, Sel, Id)) -> Id {
        crate::bootstrap();
        let class = corefoundation::base::register(name, None, std::mem::size_of::<usize>(), false);
        methods!(class, {
            "tick:" => imp, c"v24@0:8@16";
        });
        objc::object::create_instance(class, 0)
    }

    #[test]
    fn changing_fire_date_reschedules_without_changing_repeat_interval() {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn tick(_receiver: Id, _sel: Sel, _timer: Id) {
            COUNT.fetch_add(1, Ordering::SeqCst);
        }
        let target = target(c"RosemaryRescheduledTimerTarget", tick);
        let timer = make(
            60.0,
            target,
            objc::sel::get("tick:"),
            std::ptr::null_mut(),
            true,
            false,
            true,
        );
        let loop_ = corefoundation::runloop::current();
        let mode = "RosemaryRescheduledTimerMode";
        let first = now() + 0.01;
        // Exercise the actual selector registration used by the guest.
        unsafe {
            let send: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(objc::msg::entry());
            send(timer, objc::sel::get("setFireDate:"), date(first));
            assert_eq!(date_of(fire_date(timer, objc::sel::get("fireDate"))), first);
        }
        schedule_in(timer, &loop_, &[mode.into()]);
        loop_.run_in_mode(mode, 0.04, false);
        assert_eq!(COUNT.load(Ordering::SeqCst), 1);
        assert_eq!(timer_of(timer).interval, 60.0);
        unsafe {
            set_fire_date(timer, objc::sel::get("setFireDate:"), date(now() - 1.0));
        }
        loop_.run_in_mode(mode, 0.01, false);
        assert_eq!(COUNT.load(Ordering::SeqCst), 2);
        unsafe {
            invalidate(timer, objc::sel::get("invalidate"));
            set_fire_date(timer, objc::sel::get("setFireDate:"), date(now()));
        }
        schedule_in(timer, &loop_, &[mode.into()]);
        loop_.run_in_mode(mode, 0.01, false);
        assert_eq!(COUNT.load(Ordering::SeqCst), 2);
        assert_eq!(unsafe { is_valid(timer, objc::sel::get("isValid")) }, 0);
    }

    #[test]
    fn cancelled_delayed_performs_are_unscheduled() {
        let receiver = target(c"RosemaryTestCancelledPerformTarget", once);
        let tick = objc::sel::get("tick:");
        let other = objc::sel::get("description");
        // SAFETY: a live target; `tick:` takes one object.
        unsafe {
            perform_after_delay(
                receiver,
                std::ptr::null(),
                tick,
                std::ptr::null_mut(),
                1000.0,
            );
            perform_after_delay(
                receiver,
                std::ptr::null(),
                tick,
                std::ptr::null_mut(),
                1000.0,
            );
        }
        assert_eq!(pending_perform_count(receiver), 2);
        // A different selector cancels nothing.
        unsafe {
            cancel_previous_performs_matching(
                std::ptr::null_mut(),
                std::ptr::null(),
                receiver,
                other,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(pending_perform_count(receiver), 2);
        unsafe {
            cancel_previous_performs_matching(
                std::ptr::null_mut(),
                std::ptr::null(),
                receiver,
                tick,
                std::ptr::null_mut(),
            )
        };
        assert!(pending_perform_count(receiver) == 0);
        unsafe {
            perform_after_delay(
                receiver,
                std::ptr::null(),
                tick,
                std::ptr::null_mut(),
                1000.0,
            );
            cancel_previous_performs(std::ptr::null_mut(), std::ptr::null(), receiver);
        }
        assert!(pending_perform_count(receiver) == 0);
    }

    /// A deferred main-thread perform owns its target and argument until the
    /// main queue delivers the selector.
    #[test]
    fn a_selector_is_deferred_to_the_main_queue() {
        let receiver = target(c"RosemaryTestMainThreadPerformTarget", performed_on_main);
        let object = target(c"RosemaryTestMainThreadPerformObject", once);
        let expected = object as usize;
        MAIN_THREAD_ARGUMENT.store(0, Ordering::SeqCst);

        // SAFETY: both objects are live and `tick:` takes one object.
        unsafe {
            perform_on_main_thread(
                receiver,
                objc::sel::get("performSelectorOnMainThread:withObject:waitUntilDone:"),
                objc::sel::get("tick:"),
                object,
                0,
            );
        }
        assert_eq!(MAIN_THREAD_ARGUMENT.load(Ordering::SeqCst), 0);
        objc::arc::release(receiver);
        objc::arc::release(object);

        darwin::dispatch::queue::drain_main();
        assert_eq!(MAIN_THREAD_ARGUMENT.load(Ordering::SeqCst), expected);
    }

    /// A scheduled repeating timer fires from the run loop, and stops when it is
    /// invalidated.
    #[test]
    fn a_scheduled_timer_fires_until_it_is_invalidated() {
        let target = target(c"RosemaryTestRepeatingTarget", repeated);
        REPEATED.store(0, Ordering::SeqCst);
        // SAFETY: the prototype is the class method's.
        let timer = unsafe {
            scheduled_timer(
                std::ptr::null_mut(),
                objc::sel::get("scheduledTimerWithTimeInterval:"),
                0.005,
                target,
                objc::sel::get("tick:"),
                std::ptr::null_mut(),
                1,
            )
        };
        // SAFETY: `-isValid` is `BOOL (id, SEL)`.
        assert_eq!(unsafe { is_valid(timer, objc::sel::get("isValid")) }, 1);

        for _ in 0..100 {
            if REPEATED.load(Ordering::SeqCst) >= 2 {
                break;
            }
            corefoundation::runloop::current().run_in_mode(
                corefoundation::runloop::DEFAULT_MODE,
                0.01,
                false,
            );
        }
        assert!(
            REPEATED.load(Ordering::SeqCst) >= 2,
            "the timer did not repeat"
        );

        // SAFETY: `-invalidate` is `void (id, SEL)`.
        unsafe { invalidate(timer, objc::sel::get("invalidate")) };
        assert_eq!(unsafe { is_valid(timer, objc::sel::get("isValid")) }, 0);
        let after = REPEATED.load(Ordering::SeqCst);
        for _ in 0..5 {
            corefoundation::runloop::current().run_in_mode(
                corefoundation::runloop::DEFAULT_MODE,
                0.01,
                false,
            );
        }
        assert_eq!(
            REPEATED.load(Ordering::SeqCst),
            after,
            "it fired after invalidation"
        );
    }

    /// A non-repeating timer invalidates itself, which is what stops a
    /// one-shot from becoming a frame loop.
    #[test]
    fn a_one_shot_timer_stops_itself() {
        let target = target(c"RosemaryTestOneShotTarget", once);
        ONCE.store(0, Ordering::SeqCst);
        // SAFETY: the prototype is the class method's.
        let timer = unsafe {
            scheduled_timer(
                std::ptr::null_mut(),
                objc::sel::get("scheduledTimerWithTimeInterval:"),
                0.005,
                target,
                objc::sel::get("tick:"),
                std::ptr::null_mut(),
                0,
            )
        };
        for _ in 0..100 {
            if ONCE.load(Ordering::SeqCst) >= 1 {
                break;
            }
            corefoundation::runloop::current().run_in_mode(
                corefoundation::runloop::DEFAULT_MODE,
                0.01,
                false,
            );
        }
        assert_eq!(
            ONCE.load(Ordering::SeqCst),
            1,
            "a one-shot fired more than once"
        );
        // SAFETY: `-isValid` is `BOOL (id, SEL)`.
        assert_eq!(unsafe { is_valid(timer, objc::sel::get("isValid")) }, 0);
    }

    /// Cancelling by selector and argument removes only the request made with
    /// that argument; the target's other delayed performs still run.
    #[test]
    fn cancelling_a_delayed_perform_spares_other_arguments() {
        let receiver = target(c"RosemaryTestCancelledPerformTarget", record_argument);
        let kept = target(c"RosemaryTestCancelledPerformKept", once);
        let cancelled = target(c"RosemaryTestCancelledPerformDropped", once);
        CANCEL_ARGUMENTS.lock().unwrap().clear();
        let tick = objc::sel::get("tick:");
        // SAFETY: live objects; `tick:` takes one object.
        unsafe {
            for argument in [cancelled, kept] {
                perform_after_delay(receiver, std::ptr::null(), tick, argument, 0.005);
            }
            cancel_previous_performs_matching(
                std::ptr::null_mut(),
                std::ptr::null(),
                receiver,
                tick,
                cancelled,
            );
        }
        for _ in 0..100 {
            if !CANCEL_ARGUMENTS.lock().unwrap().is_empty() {
                break;
            }
            corefoundation::runloop::current().run_in_mode(
                corefoundation::runloop::DEFAULT_MODE,
                0.01,
                false,
            );
        }
        assert_eq!(*CANCEL_ARGUMENTS.lock().unwrap(), [kept as usize]);
    }

    /// Cancelling by target alone removes every delayed perform on it.
    #[test]
    fn cancelling_by_target_removes_every_delayed_perform() {
        let receiver = target(c"RosemaryTestCancelAllTarget", count_cancel_all);
        CANCEL_ALL.store(0, Ordering::SeqCst);
        // SAFETY: a live object; `tick:` takes one object.
        unsafe {
            for _ in 0..2 {
                perform_after_delay(
                    receiver,
                    std::ptr::null(),
                    objc::sel::get("tick:"),
                    std::ptr::null_mut(),
                    0.005,
                );
            }
            cancel_previous_performs(std::ptr::null_mut(), std::ptr::null(), receiver);
        }
        for _ in 0..5 {
            corefoundation::runloop::current().run_in_mode(
                corefoundation::runloop::DEFAULT_MODE,
                0.01,
                false,
            );
        }
        assert_eq!(CANCEL_ALL.load(Ordering::SeqCst), 0);
    }

    /// A date is seconds since Apple's epoch, and the Unix conversion is the
    /// difference between the two - which is the one number here that can be
    /// wrong by thirty years without looking wrong.
    #[test]
    fn a_date_agrees_with_the_unix_epoch() {
        crate::bootstrap();
        let d = date(0.0);
        // SAFETY: both are `double (id, SEL)`.
        unsafe {
            assert_eq!(
                time_interval_since_1970(d, objc::sel::get("timeIntervalSince1970")),
                UNIX_TO_REFERENCE
            );
            assert_eq!(
                time_interval_since_reference(d, objc::sel::get("timeIntervalSinceReferenceDate")),
                0.0
            );
        }
    }

    /// Something unique to the calling thread while it runs.
    fn thread_token() -> usize {
        thread_local! {
            static TOKEN: u8 = const { 0 };
        }
        TOKEN.with(|token| token as *const u8 as usize)
    }

    unsafe extern "C" fn record_thread(info: *mut c_void) {
        // SAFETY: `info` is one of the tests' statics.
        unsafe { &*(info as *const AtomicUsize) }.store(thread_token(), Ordering::SeqCst);
    }

    static TIMER_THREAD: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn timer_records_thread(_receiver: Id, _sel: Sel, _timer: Id) {
        TIMER_THREAD.store(thread_token(), Ordering::SeqCst);
    }

    /// `+currentRunLoop` is the calling thread's own loop, the one
    /// `CFRunLoopGetCurrent` names there, while `+mainRunLoop` is one object
    /// from every thread.
    #[test]
    fn each_thread_has_its_own_run_loop() {
        crate::bootstrap();
        let ask = || {
            // SAFETY: class methods and `-getCFRunLoop`, which take nothing.
            unsafe {
                let current = current_run_loop(std::ptr::null_mut(), std::ptr::null());
                let again = current_run_loop(std::ptr::null_mut(), std::ptr::null());
                let main = main_run_loop(std::ptr::null_mut(), std::ptr::null());
                assert_eq!(current, again, "a second ask made a second object");
                assert_eq!(
                    get_cf_run_loop(current, std::ptr::null()) as usize,
                    corefoundation::api::CFRunLoopGetCurrent() as usize
                );
                assert_eq!(
                    get_cf_run_loop(main, std::ptr::null()) as usize,
                    corefoundation::api::CFRunLoopGetMain() as usize
                );
                (current as usize, main as usize)
            }
        };
        let (one, main_one) = std::thread::spawn(ask).join().unwrap();
        let (two, main_two) = std::thread::spawn(ask).join().unwrap();
        assert_ne!(one, two, "two threads were handed one run loop");
        assert_ne!(one, main_one, "a secondary thread was handed the main loop");
        assert_eq!(main_one, main_two);
    }

    /// A secondary thread turning its own `NSRunLoop` services its own sources
    /// and timers, and never the main loop's: those are the main thread's
    /// work, and running them beside it is a race the program never agreed to.
    #[test]
    fn a_secondary_thread_runs_its_own_loop_and_not_the_main_one() {
        static MAIN_SOURCE: AtomicUsize = AtomicUsize::new(0);
        static OWN_SOURCE: AtomicUsize = AtomicUsize::new(0);
        crate::bootstrap();
        let main_source = corefoundation::runloop::new_source(
            Some(record_thread),
            &MAIN_SOURCE as *const AtomicUsize as usize,
        );
        corefoundation::runloop::main()
            .add_source(&main_source, corefoundation::runloop::DEFAULT_MODE);
        main_source.signal();
        let own_source = corefoundation::runloop::new_source(
            Some(record_thread),
            &OWN_SOURCE as *const AtomicUsize as usize,
        );
        corefoundation::runloop::current()
            .add_source(&own_source, corefoundation::runloop::DEFAULT_MODE);
        own_source.signal();

        let target = target(c"RosemaryTestThreadTimerTarget", timer_records_thread);
        // SAFETY: the prototypes are the methods'.
        unsafe {
            scheduled_timer(
                std::ptr::null_mut(),
                std::ptr::null(),
                0.005,
                target,
                objc::sel::get("tick:"),
                std::ptr::null_mut(),
                0,
            );
            let run_loop = current_run_loop(std::ptr::null_mut(), std::ptr::null());
            for _ in 0..100 {
                if TIMER_THREAD.load(Ordering::SeqCst) != 0 {
                    break;
                }
                run_until_date(run_loop, std::ptr::null(), date(now() + 0.01));
            }
        }
        corefoundation::runloop::main()
            .remove_source(&main_source, corefoundation::runloop::DEFAULT_MODE);
        main_source.invalidate();

        let me = thread_token();
        assert_eq!(
            OWN_SOURCE.load(Ordering::SeqCst),
            me,
            "its own source did not fire"
        );
        assert_eq!(
            TIMER_THREAD.load(Ordering::SeqCst),
            me,
            "its timer fired elsewhere"
        );
        assert_ne!(
            MAIN_SOURCE.load(Ordering::SeqCst),
            me,
            "it serviced the main loop's source"
        );
    }

    static MODE_TICKS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn mode_tick(_receiver: Id, _sel: Sel, _timer: Id) {
        MODE_TICKS.fetch_add(1, Ordering::SeqCst);
    }

    /// `-addTimer:forMode:` puts the timer in that mode: it fires while the
    /// mode runs and not while only the default one does, and a timer added for
    /// a second mode fires in either.
    #[test]
    fn a_timer_added_for_a_mode_fires_in_that_mode() {
        let target = target(c"RosemaryTestModeTimerTarget", mode_tick);
        MODE_TICKS.store(0, Ordering::SeqCst);
        let tracking = crate::string::from_str("RosemaryTestTrackingMode");
        // SAFETY: the prototypes are the methods'.
        unsafe {
            let timer = unscheduled_timer(
                std::ptr::null_mut(),
                std::ptr::null(),
                0.001,
                target,
                objc::sel::get("tick:"),
                std::ptr::null_mut(),
                1,
            );
            let run_loop = current_run_loop(std::ptr::null_mut(), std::ptr::null());
            add_timer_for_mode(run_loop, std::ptr::null(), timer, tracking);

            run_mode_before_date(
                run_loop,
                std::ptr::null(),
                std::ptr::null_mut(),
                date(now() + 0.02),
            );
            assert_eq!(
                MODE_TICKS.load(Ordering::SeqCst),
                0,
                "it fired in the default mode"
            );
            for _ in 0..100 {
                if MODE_TICKS.load(Ordering::SeqCst) != 0 {
                    break;
                }
                run_mode_before_date(run_loop, std::ptr::null(), tracking, date(now() + 0.01));
            }
            assert_ne!(
                MODE_TICKS.load(Ordering::SeqCst),
                0,
                "it did not fire in its mode"
            );

            add_timer_for_mode(run_loop, std::ptr::null(), timer, std::ptr::null_mut());
            let before = MODE_TICKS.load(Ordering::SeqCst);
            for _ in 0..100 {
                if MODE_TICKS.load(Ordering::SeqCst) != before {
                    break;
                }
                run_mode_before_date(
                    run_loop,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    date(now() + 0.01),
                );
            }
            assert_ne!(
                MODE_TICKS.load(Ordering::SeqCst),
                before,
                "the second mode was not added"
            );

            invalidate(timer, objc::sel::get("invalidate"));
        }
    }
}
