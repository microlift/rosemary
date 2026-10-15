use std::ffi::c_void;
use std::sync::Arc;

use objc::object::Id;
use objc::sel::Sel;

use crate::define::{class_methods, methods};

#[repr(C)]
pub struct DisplayLink {
    isa: *const c_void,
    target: Id,
    selector: Sel,
    display: usize,
    timer: *const c_void,
    timestamp: f64,
    target_timestamp: f64,
    frame_interval: isize,
    preferred_fps: isize,
    range: FrameRateRange,
    paused: u8,
    invalidated: u8,
}

/// `CAFrameRateRange`: three `float`s, which AAPCS64 passes and returns in
/// `s0`-`s2`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct FrameRateRange {
    minimum: f32,
    maximum: f32,
    preferred: f32,
}

/// `CAFrameRateRange CAFrameRateRangeMake(float, float, float)`.
///
/// Weak-imported by programs that run on older systems, so a null here is a
/// call through nothing the moment one uses it.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CAFrameRateRangeMake(
    minimum: f32,
    maximum: f32,
    preferred: f32,
) -> FrameRateRange {
    FrameRateRange {
        minimum,
        maximum,
        preferred,
    }
}

pub const INSTANCE_SIZE: usize = size_of::<DisplayLink>();

fn link_of<'a>(receiver: Id) -> &'a mut DisplayLink {
    // SAFETY: a display link of this module's.
    unsafe { &mut *(receiver as *mut DisplayLink) }
}

fn now() -> f64 {
    darwin::mach::media_time()
}

/// The seconds between calls: the preferred rate if the guest set one the
/// display can honour, else a whole number of refreshes.
fn interval_of(link: &DisplayLink) -> f64 {
    let refresh = 1.0 / refresh_rate_of(link);
    let by_rate = (link.preferred_fps > 0).then(|| 1.0 / link.preferred_fps as f64);
    by_rate
        .map(|i| i.max(refresh))
        .unwrap_or(refresh * link.frame_interval.max(1) as f64)
}

fn refresh_rate_of(link: &DisplayLink) -> f64 {
    sdl3::video::display_refresh_rate(link.display as u32)
        .filter(|rate| rate.is_finite() && *rate > 0.0)
        .map(f64::from)
        .unwrap_or(60.0)
}

/// The refresh a call at `when` is for and the one after it, as `-timestamp`
/// and `-targetTimestamp`.
///
/// Refreshes are `interval` apart on a fixed grid, and the two stamps are
/// points on it however late the call itself is: Apple's are the display's own
/// times, and a program pacing its frames by them - Roblox keeps a history of
/// them to match against what it presented - reads the jitter of a stamp taken
/// from the clock as the display stuttering. `last` keeps them advancing when a
/// call arrives a little before its refresh.
fn refresh_around(when: f64, interval: f64, last: f64) -> (f64, f64) {
    // `interval_of` never gives this; a zero would divide below and a NaN
    // would panic in `Duration`, so the stamps degrade to the plain clock.
    if !(interval.is_finite() && interval > 0.0) {
        return (when, when);
    }
    // A call woken a hair early belongs to the refresh it was set for.
    let mut shown = (when / interval + 0.05).floor() * interval;
    if shown <= last {
        shown = last + interval;
    }
    (shown, shown + interval)
}

/// One turn of a link: stamp it and send the selector to the target.
unsafe extern "C" fn fire(_timer: *const c_void, info: *mut c_void) {
    let object = info as Id;
    let (target, selector, interval) = {
        let link = link_of(object);
        if link.paused != 0 || link.target.is_null() {
            return;
        }
        (link.target, link.selector, interval_of(link))
    };
    {
        let link = link_of(object);
        // Read together, so the wait below is measured from the same instant.
        let (when, at) = (now(), std::time::Instant::now());
        let (shown, next) = refresh_around(when, interval, link.timestamp);
        link.timestamp = shown;
        link.target_timestamp = next;
        // Due again at that refresh, not an interval after whenever the loop
        // got round to this one: the lateness would otherwise add up, and the
        // link would run slower than the display it stands for.
        if !link.timer.is_null() {
            // SAFETY: the `Arc` `schedule` leaked into the object, still held.
            let timer = unsafe { &*(link.timer as *const corefoundation::runloop::Timer) };
            let wait = (next - when).max(0.0);
            if wait.is_finite() {
                timer.set_next(at + std::time::Duration::from_secs_f64(wait));
            }
        }
    }
    // SAFETY: a live object this runtime allocated.
    let method = unsafe { objc::object::class_of(target) }.and_then(|c| c.method(selector));
    let Some(method) = method else {
        // Named rather than silent: a display link that fires into nothing is
        // a frame loop that does not run.
        objc::fail!("-[CADisplayLink]: the target does not respond to its selector");
    };
    objc::arc::retain(object);
    // SAFETY: a display link action is `void (id, SEL, CADisplayLink *)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, Id) = std::mem::transmute(method.implementation());
        f(target, selector, object);
    }
    objc::arc::release(object);
}

/// Put the link on the main loop at its current interval.
fn schedule(object: Id) {
    let link = link_of(object);
    if !link.timer.is_null() || link.invalidated != 0 {
        return;
    }
    let interval = std::time::Duration::from_secs_f64(interval_of(link));
    let timer =
        corefoundation::runloop::new_timer(interval, Some(interval), Some(fire), object as usize);
    corefoundation::runloop::main().add_timer(&timer, corefoundation::runloop::DEFAULT_MODE);
    // The run loop's own reference, given back by `unschedule`.
    objc::arc::retain(object);
    link.timer = Arc::into_raw(timer) as *const c_void;
}

/// Take the link off the loop; true if it was on one.
fn unschedule(object: Id) -> bool {
    let link = link_of(object);
    if link.timer.is_null() {
        return false;
    }
    // SAFETY: an `Arc` `schedule` leaked into the object, taken back once
    // because the pointer is cleared here.
    let timer = unsafe { Arc::from_raw(link.timer as *const corefoundation::runloop::Timer) };
    link.timer = std::ptr::null();
    corefoundation::runloop::main().remove_timer(&timer, corefoundation::runloop::DEFAULT_MODE);
    timer.invalidate();
    objc::arc::release(object);
    true
}

/// A rate change takes effect on the next call, as it does for Apple's.
fn reschedule(object: Id) {
    // The run loop holds a reference to `object` while scheduled. Retain
    // across the removal so that a link whose only reference is the run
    // loop's is not deallocated before being re-scheduled.
    objc::arc::retain(object);
    if unschedule(object) {
        schedule(object);
    }
    objc::arc::release(object);
}

/// The display a link follows: a screen's own, and for a view or window the
/// first one, because nothing here tracks which display a window is on.
fn display_of_receiver(receiver: Id) -> u32 {
    // SAFETY: a live object this runtime allocated.
    let is_screen = unsafe { crate::screen::display_id(receiver) }.is_some()
        || unsafe { objc::object::class_of(receiver) }
            .is_some_and(|c| objc::class::lookup(c"UIScreen").is_some_and(|k| std::ptr::eq(c, k)));
    if is_screen {
        return crate::screen::display_of(receiver);
    }
    sdl3::video::displays().first().copied().unwrap_or(0)
}

unsafe extern "C" fn display_link_with_target(
    receiver: Id,
    _sel: Sel,
    target: Id,
    selector: Sel,
) -> Id {
    let class = objc::class::lookup(c"CADisplayLink").expect("registered by this module");
    // SAFETY: `DisplayLink` is the layout the class was registered with.
    let object = unsafe { corefoundation::base::allocate::<DisplayLink>(class) };
    // SAFETY: freshly allocated; every field is written before it is read.
    unsafe {
        (*object).target = objc::arc::retain(target);
        (*object).selector = selector;
        (*object).display = display_of_receiver(receiver) as usize;
        (*object).timer = std::ptr::null();
        (*object).timestamp = 0.0;
        (*object).target_timestamp = 0.0;
        (*object).frame_interval = 1;
        (*object).preferred_fps = 0;
        (*object).range = FrameRateRange::default();
        (*object).paused = 0;
        (*object).invalidated = 0;
    }
    objc::arc::autorelease(object as Id)
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // A scheduled link holds a reference of the run loop's, so reaching here
    // means it is on none.
    objc::arc::release(link_of(receiver).target);
    objc::arc::finalize(receiver);
}

unsafe extern "C" fn add_to_run_loop(receiver: Id, _sel: Sel, _run_loop: Id, _mode: Id) {
    // One loop and one mode in this runtime: see `NSRunLoop`'s `addTimer:`.
    schedule(receiver);
}

unsafe extern "C" fn remove_from_run_loop(receiver: Id, _sel: Sel, _run_loop: Id, _mode: Id) {
    unschedule(receiver);
}

/// `-invalidate`: off every loop, and the target released. Not reversible.
unsafe extern "C" fn invalidate(receiver: Id, _sel: Sel) {
    let link = link_of(receiver);
    if link.invalidated != 0 {
        return;
    }
    link.invalidated = 1;
    // The loop's reference is what may be keeping `receiver` alive, so take
    // one of our own across the removal.
    objc::arc::retain(receiver);
    unschedule(receiver);
    objc::arc::release(link.target);
    link.target = std::ptr::null_mut();
    objc::arc::release(receiver);
}

unsafe extern "C" fn is_paused(receiver: Id, _sel: Sel) -> u8 {
    link_of(receiver).paused
}

unsafe extern "C" fn set_paused(receiver: Id, _sel: Sel, paused: u8) {
    link_of(receiver).paused = (paused != 0) as u8;
}

unsafe extern "C" fn timestamp(receiver: Id, _sel: Sel) -> f64 {
    link_of(receiver).timestamp
}

unsafe extern "C" fn target_timestamp(receiver: Id, _sel: Sel) -> f64 {
    link_of(receiver).target_timestamp
}

/// `-duration`: the display's refresh period, which is not the call interval
/// when the link is throttled.
unsafe extern "C" fn duration(receiver: Id, _sel: Sel) -> f64 {
    1.0 / refresh_rate_of(link_of(receiver))
}

unsafe extern "C" fn frame_interval(receiver: Id, _sel: Sel) -> isize {
    link_of(receiver).frame_interval
}

unsafe extern "C" fn set_frame_interval(receiver: Id, _sel: Sel, value: isize) {
    // Zero or less is not a rate; Apple's keeps the old one.
    if value >= 1 {
        link_of(receiver).frame_interval = value;
        reschedule(receiver);
    }
}

unsafe extern "C" fn preferred_frames_per_second(receiver: Id, _sel: Sel) -> isize {
    link_of(receiver).preferred_fps
}

unsafe extern "C" fn set_preferred_frames_per_second(receiver: Id, _sel: Sel, value: isize) {
    link_of(receiver).preferred_fps = value.max(0);
    reschedule(receiver);
}

unsafe extern "C" fn preferred_frame_rate_range(receiver: Id, _sel: Sel) -> FrameRateRange {
    link_of(receiver).range
}

/// `-setPreferredFrameRateRange:`. The rate to aim for is `preferred`, else the
/// top of the range; zero in both is the display's own rate.
unsafe extern "C" fn set_preferred_frame_rate_range(
    receiver: Id,
    _sel: Sel,
    range: FrameRateRange,
) {
    let link = link_of(receiver);
    link.range = range;
    let rate = [range.preferred, range.maximum]
        .into_iter()
        .find(|r| r.is_finite() && *r > 0.0);
    link.preferred_fps = rate.map_or(0, |r| r.round() as isize);
    reschedule(receiver);
}

/// `+[UIScreen mainScreen]`.
unsafe extern "C" fn main_screen(_class: Id, _sel: Sel) -> Id {
    match sdl3::video::displays().first() {
        Some(display) => crate::screen::screen_object(c"UIScreen", *display),
        // No display is nil, as for `NSScreen`.
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn maximum_frames_per_second(receiver: Id, _sel: Sel) -> isize {
    crate::screen::refresh_rate(receiver).round().max(1.0) as isize
}

pub(crate) fn install() {
    let screen = objc::class::lookup(c"UIScreen").expect("declared before its methods");
    class_methods!(screen, {
        "mainScreen" => main_screen, c"@16@0:8";
    });
    methods!(screen, {
        "maximumFramesPerSecond" => maximum_frames_per_second, c"q16@0:8";
        "displayLinkWithTarget:selector:" => display_link_with_target, c"@32@0:8@16:24";
    });

    // macOS 14 made it a method of the three, with the same meaning.
    for name in [c"NSScreen", c"NSView", c"NSWindow"] {
        let class = objc::class::lookup(name).expect("declared before its methods");
        methods!(class, {
            "displayLinkWithTarget:selector:" => display_link_with_target, c"@32@0:8@16:24";
        });
    }

    let link = objc::class::lookup(c"CADisplayLink").expect("declared before its methods");
    methods!(link, {
        "dealloc" => dealloc, c"v16@0:8";
        "addToRunLoop:forMode:" => add_to_run_loop, c"v32@0:8@16@24";
        "removeFromRunLoop:forMode:" => remove_from_run_loop, c"v32@0:8@16@24";
        "invalidate" => invalidate, c"v16@0:8";
        "isPaused" => is_paused, c"B16@0:8";
        "setPaused:" => set_paused, c"v20@0:8B16";
        "timestamp" => timestamp, c"d16@0:8";
        "targetTimestamp" => target_timestamp, c"d16@0:8";
        "duration" => duration, c"d16@0:8";
        "frameInterval" => frame_interval, c"q16@0:8";
        "setFrameInterval:" => set_frame_interval, c"v24@0:8q16";
        "preferredFramesPerSecond" => preferred_frames_per_second, c"q16@0:8";
        "preferredFrameRateRange" => preferred_frame_rate_range,
            c"{CAFrameRateRange=fff}16@0:8";
        "setPreferredFrameRateRange:" => set_preferred_frame_rate_range,
            c"v28@0:8{CAFrameRateRange=fff}16";
        "setPreferredFramesPerSecond:" => set_preferred_frames_per_second, c"v24@0:8q16";
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn tick(_receiver: Id, _sel: Sel, link: Id) {
        assert!(link_of(link).timestamp > 0.0);
        CALLS.fetch_add(1, Ordering::SeqCst);
    }

    fn send(object: Id, name: &str) -> Id {
        let sel = objc::sel::get(name);
        // SAFETY: the selectors used are `id (id, SEL)` or `void` ones.
        let method = unsafe { objc::object::class_of(object) }
            .and_then(|c| c.method(sel))
            .expect("registered");
        let f: unsafe extern "C" fn(Id, Sel) -> Id =
            unsafe { std::mem::transmute(method.implementation()) };
        unsafe { f(object, sel) }
    }

    #[test]
    fn stamps_are_refreshes_however_late_the_call_is() {
        let interval = 0.25;
        // On time, late, and a hair early: all the refresh at 10.0.
        assert_eq!(refresh_around(10.0, interval, 9.75), (10.0, 10.25));
        assert_eq!(refresh_around(10.2, interval, 9.75), (10.0, 10.25));
        assert_eq!(refresh_around(9.995, interval, 9.75), (10.0, 10.25));
        // A second call inside one refresh still moves on.
        assert_eq!(refresh_around(10.1, interval, 10.0), (10.25, 10.5));
    }

    #[test]
    fn stamps_stay_on_the_grid_at_real_refresh_rates() {
        for rate in [60.0, 120.0] {
            let interval = 1.0 / rate;
            let mut last = 0.0;
            for n in 1000..1100 {
                let grid = n as f64 * interval;
                // Up to 40% of a refresh late, and 4% early (inside the slack).
                for skew in [0.0, 0.4 * interval, -0.04 * interval] {
                    let (shown, next) = refresh_around(grid + skew, interval, last);
                    assert!((shown - grid).abs() < 1e-9, "{rate} {n} {skew}");
                    assert!((next - shown - interval).abs() < 1e-9);
                }
                last = grid;
            }
        }
    }

    #[test]
    fn a_degenerate_interval_does_not_panic() {
        for interval in [0.0, f64::NAN, f64::INFINITY, -1.0] {
            let (shown, next) = refresh_around(5.0, interval, 1.0);
            assert_eq!((shown, next), (5.0, 5.0));
        }
    }

    #[test]
    fn a_scheduled_link_calls_its_target_until_invalidated() {
        crate::bootstrap();
        let target_class = corefoundation::base::register(
            c"DisplayLinkTestTarget",
            None,
            size_of::<usize>(),
            false,
        );
        let selector = objc::sel::get("tick:");
        target_class.replace_method(selector, tick as *const c_void, c"v24@0:8@16");
        // SAFETY: the class was registered with room for its `isa`.
        let target = unsafe { corefoundation::base::allocate::<usize>(target_class) } as Id;

        let screen = match crate::screen::screen_object(c"UIScreen", 0) {
            s if !s.is_null() => s,
            _ => return,
        };
        let make = objc::sel::get("displayLinkWithTarget:selector:");
        let method = unsafe { objc::object::class_of(screen) }
            .and_then(|c| c.method(make))
            .unwrap();
        let f: unsafe extern "C" fn(Id, Sel, Id, Sel) -> Id =
            unsafe { std::mem::transmute(method.implementation()) };
        let link = unsafe { f(screen, make, target, selector) };
        objc::arc::retain(link);

        let before = CALLS.load(Ordering::SeqCst);
        schedule(link);
        // The main loop is shared with every other test in the process, and
        // another one stopping it ends a run early; so run until it has fired.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while CALLS.load(Ordering::SeqCst) == before && std::time::Instant::now() < deadline {
            corefoundation::runloop::main().run_in_mode(
                corefoundation::runloop::DEFAULT_MODE,
                0.05,
                false,
            );
        }
        let during = CALLS.load(Ordering::SeqCst);
        assert!(during > before, "the link never fired");
        assert!(link_of(link).target_timestamp > link_of(link).timestamp);

        send(link, "invalidate");
        corefoundation::runloop::main().run_in_mode(
            corefoundation::runloop::DEFAULT_MODE,
            0.1,
            false,
        );
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            during,
            "fired after invalidate"
        );
        objc::arc::release(link);
    }

    #[test]
    fn reschedule_does_not_deallocate_when_only_held_by_runloop() {
        crate::bootstrap();
        let target_class = corefoundation::base::register(
            c"DisplayLinkRescheduleTarget",
            None,
            size_of::<usize>(),
            false,
        );
        let selector = objc::sel::get("tick:");
        target_class.replace_method(selector, tick as *const c_void, c"v24@0:8@16");
        let target = unsafe { corefoundation::base::allocate::<usize>(target_class) } as Id;

        let screen = match crate::screen::screen_object(c"UIScreen", 0) {
            s if !s.is_null() => s,
            _ => return,
        };
        let make = objc::sel::get("displayLinkWithTarget:selector:");
        let method = unsafe { objc::object::class_of(screen) }
            .and_then(|c| c.method(make))
            .unwrap();
        let f: unsafe extern "C" fn(Id, Sel, Id, Sel) -> Id =
            unsafe { std::mem::transmute(method.implementation()) };
        let link = unsafe { f(screen, make, target, selector) };

        schedule(link);
        // Release the initial allocation reference so that only the run loop retains it:
        objc::arc::release(link);

        let set_fps = objc::sel::get("setPreferredFramesPerSecond:");
        let method = unsafe { objc::object::class_of(link) }
            .and_then(|c| c.method(set_fps))
            .unwrap();
        let f: unsafe extern "C" fn(Id, Sel, isize) =
            unsafe { std::mem::transmute(method.implementation()) };
        unsafe { f(link, set_fps, 30) };

        assert!(!link_of(link).timer.is_null());
        send(link, "invalidate");
    }
}
