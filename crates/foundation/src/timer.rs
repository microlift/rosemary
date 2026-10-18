use std::ffi::c_void;
use std::sync::Arc;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

/// Seconds between Apple's epoch, 2001-01-01, and the Unix one.
const UNIX_TO_REFERENCE: f64 = 978_307_200.0;

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
    selector: Sel,
    interval: f64,
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
unsafe extern "C" fn fire(_timer: *const c_void, info: *mut c_void) {
    let object = info as Id;
    let (target, selector, repeats, user_info, passes_timer) = {
        let timer = timer_of(object);
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

/// Build a timer object, scheduling it on the main loop if asked.
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
        (*object).repeats = repeats as usize;
        (*object).passes_timer = passes_timer as usize;
        (*object).timer = std::ptr::null();
    }
    let object = object as Id;
    if schedule {
        add_to_main_loop(object);
    }
    objc::arc::autorelease(object)
}

/// Schedule an already-built timer on the main run loop.
fn add_to_main_loop(object: Id) {
    let timer = timer_of(object);
    if !timer.timer.is_null() {
        return;
    }
    let interval = std::time::Duration::from_secs_f64(timer.interval);
    let repeat = (timer.repeats != 0).then_some(interval);
    let run_loop_timer =
        corefoundation::runloop::new_timer(interval, repeat, Some(fire), object as usize);
    corefoundation::runloop::main()
        .add_timer(&run_loop_timer, corefoundation::runloop::DEFAULT_MODE);
    // Kept alive by the object, and by the table: a scheduled timer outlives
    // the reference the program holds.
    objc::arc::retain(object);
    SCHEDULED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(object as usize);
    timer.timer = Arc::into_raw(run_loop_timer) as *const c_void;
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
    if timer.timer.is_null() {
        return;
    }
    // SAFETY: an `Arc` this module leaked into the object; taken back exactly
    // once, because the pointer is cleared below.
    let run_loop_timer =
        unsafe { Arc::from_raw(timer.timer as *const corefoundation::runloop::Timer) };
    timer.timer = std::ptr::null();
    corefoundation::runloop::main()
        .remove_timer(&run_loop_timer, corefoundation::runloop::DEFAULT_MODE);
    run_loop_timer.invalidate();
    SCHEDULED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|t| *t != receiver as usize);
    objc::arc::release(receiver);
}

unsafe extern "C" fn is_valid(receiver: Id, _sel: Sel) -> u8 {
    !timer_of(receiver).timer.is_null() as u8
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
    date(now() + timer_of(receiver).interval)
}

/// The one run loop object, which `+currentRunLoop` and `+mainRunLoop` answer.
///
/// One rather than one per thread: `corefoundation::runloop::main` is what every
/// method here acts on, so two objects would be two names for the same loop and
/// a program comparing them would be told they were different.
static RUN_LOOP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

unsafe extern "C" fn current_run_loop(class: Id, _sel: Sel) -> Id {
    *RUN_LOOP.get_or_init(|| {
        // SAFETY: `class` is `NSRunLoop`, which this module registered.
        let object = unsafe {
            objc::object::create_instance(
                objc::class::from_ptr(class as *const objc::class::Class).expect("a class"),
                0,
            )
        };
        objc::arc::retain(object);
        object as usize
    }) as Id
}

unsafe extern "C" fn add_timer_for_mode(_receiver: Id, _sel: Sel, timer: Id, _mode: Id) {
    if !timer.is_null() {
        add_to_main_loop(timer);
    }
}

/// `-runMode:beforeDate:`, which is `CFRunLoopRunInMode` under another name.
unsafe extern "C" fn run_mode_before_date(_receiver: Id, _sel: Sel, _mode: Id, limit: Id) -> u8 {
    let seconds = match limit.is_null() {
        true => 0.0,
        false => (date_of(limit) - now()).max(0.0),
    };
    let result = corefoundation::runloop::main().run_in_mode(
        corefoundation::runloop::DEFAULT_MODE,
        seconds,
        true,
    );
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
        "performSelectorOnMainThread:withObject:waitUntilDone:" => perform_on_main_thread,
            c"v40@0:8:16@24c32";
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
    });

    let run_loop =
        corefoundation::base::register(c"NSRunLoop", None, std::mem::size_of::<usize>(), false);
    class_methods!(run_loop, {
        "currentRunLoop" => current_run_loop, c"@16@0:8";
        "mainRunLoop" => current_run_loop, c"@16@0:8";
    });
    methods!(run_loop, {
        "addTimer:forMode:" => add_timer_for_mode, c"v32@0:8@16@24";
        "runMode:beforeDate:" => run_mode_before_date, c"B32@0:8@16@24";
        "runUntilDate:" => run_until_date, c"v24@0:8@16";
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

#[cfg(test)]
mod tests {
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

    unsafe extern "C" fn performed_on_main(_receiver: Id, _sel: Sel, object: Id) {
        MAIN_THREAD_ARGUMENT.store(object as usize, Ordering::SeqCst);
    }

    /// A target of this test's own.
    ///
    /// Two, one per test, because the tests share a process *and* the one run
    /// loop: a timer another test scheduled fires while this one is turning the
    /// loop, and a shared counter would count both.
    fn target(name: &std::ffi::CStr, imp: unsafe extern "C" fn(Id, Sel, Id)) -> Id {
        crate::bootstrap();
        let class = corefoundation::base::register(name, None, std::mem::size_of::<usize>(), false);
        methods!(class, {
            "tick:" => imp, c"v24@0:8@16";
        });
        objc::object::create_instance(class, 0)
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
            corefoundation::runloop::main().run_in_mode(
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
            corefoundation::runloop::main().run_in_mode(
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
            corefoundation::runloop::main().run_in_mode(
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
}
