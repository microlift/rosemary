//! `NSSlider` and `NSSliderCell`: a linear slider's value and range, where its
//! parts lie, and how they are drawn.
//!
//! The slider keeps the state and the cell answers the geometry, as in AppKit;
//! a toolkit drawing its own sliders (Qt's macOS style) configures an
//! `NSSlider`, asks its cell for the knob and bar rectangles, and has the cell
//! draw each part into its own context.
//!
//! FIXME (ACCURACY): a slider does not track the mouse, so one a program puts
//! in a window shows its value but cannot be dragged.

use foundation::geometry::{NSPoint, NSRect};
use objc::object::Id;
use objc::sel::Sel;

use crate::aqua::{self, SliderLayout, TickSide};
use crate::define::methods;
use crate::view;

/// `NSTickMarkPosition`: below and trailing are 0, above and leading 1.
const TICK_MARK_ABOVE: usize = 1;

#[repr(C)]
struct Slider {
    /// Clear in a slider no initialiser of this module's has set up, which
    /// [`slider_of`] then gives AppKit's defaults.
    initialised: bool,
    min: f64,
    max: f64,
    value: f64,
    tick_marks: isize,
    tick_mark_position: usize,
    /// `None` until a program says, when the frame's shape decides.
    vertical: Option<bool>,
    /// Between `-startTrackingAt:inView:` and `-stopTracking:...`, when the
    /// knob is drawn pressed.
    tracking: bool,
}

const DEFAULT: Slider = Slider {
    initialised: true,
    min: 0.0,
    max: 1.0,
    value: 0.0,
    tick_marks: 0,
    tick_mark_position: 0,
    vertical: None,
    tracking: false,
};

pub const INSTANCE_SIZE: usize = view::INSTANCE_SIZE + size_of::<Slider>();

fn slider_of<'a>(receiver: Id) -> &'a mut Slider {
    let base = objc::class::lookup(c"NSView")
        .expect("NSView is declared before NSSlider")
        .instance_size();
    // SAFETY: NSSlider adds exactly this body after NSView's storage, and every
    // subclass inherits it at the same offset.
    let slider = unsafe { &mut *receiver.cast::<u8>().add(base).cast::<Slider>() };
    if !slider.initialised {
        *slider = DEFAULT;
    }
    slider
}

fn is_vertical(receiver: Id) -> bool {
    let size = view::bounds_of(receiver).size;
    slider_of(receiver)
        .vertical
        .unwrap_or(size.height > size.width)
}

/// The value's share of the range, 0 for an empty range.
fn fraction(receiver: Id) -> f64 {
    let s = slider_of(receiver);
    match s.max > s.min {
        true => ((s.value - s.min) / (s.max - s.min)).clamp(0.0, 1.0),
        false => 0.0,
    }
}

/// The slider's layout over its bounds, y-down.
fn layout(receiver: Id) -> SliderLayout {
    let s = slider_of(receiver);
    let ticks = (s.tick_marks > 0).then_some(match s.tick_mark_position {
        TICK_MARK_ABOVE => TickSide::Before,
        _ => TickSide::After,
    });
    SliderLayout {
        frame: view::bounds_of(receiver),
        horizontal: !is_vertical(receiver),
        size: aqua::Size::from_control_size(crate::props::integer(receiver, "controlSize")),
        ticks,
    }
}

/// `r`, laid out y-down over the slider's bounds, in the slider's coordinates
/// when they are not `flipped`.
fn oriented(receiver: Id, r: NSRect, flipped: bool) -> NSRect {
    if flipped {
        return r;
    }
    let bounds = view::bounds_of(receiver);
    let y = 2.0 * bounds.origin.y + bounds.size.height - r.origin.y - r.size.height;
    NSRect {
        origin: NSPoint { x: r.origin.x, y },
        size: r.size,
    }
}

fn look(receiver: Id) -> aqua::Look {
    // SAFETY: `-isEnabled` is `BOOL (id, SEL)` on every control.
    let enabled = unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("isEnabled")) != 0
    };
    aqua::Look {
        pressed: slider_of(receiver).tracking,
        disabled: !enabled,
        ..aqua::Look::default()
    }
}

/// `-initWithFrame:`: AppKit's defaults, then `NSView`'s initialiser. A
/// program may send it again to an initialised slider to start over, as Qt's
/// style does before measuring each slider.
unsafe extern "C" fn init_with_frame(receiver: Id, sel: Sel, frame: NSRect) -> Id {
    *slider_of(receiver) = DEFAULT;
    // Reached by its class rather than by sending: a send would find this
    // override again.
    let Some(method) = objc::class::lookup(c"NSView")
        .and_then(|class| class.method(objc::sel::get("initWithFrame:")))
    else {
        return std::ptr::null_mut();
    };
    // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, NSRect) -> Id =
            std::mem::transmute(method.implementation());
        f(receiver, sel, frame)
    }
}

unsafe extern "C" fn min_value(receiver: Id, _sel: Sel) -> f64 {
    slider_of(receiver).min
}

unsafe extern "C" fn set_min_value(receiver: Id, _sel: Sel, value: f64) {
    slider_of(receiver).min = value;
}

unsafe extern "C" fn max_value(receiver: Id, _sel: Sel) -> f64 {
    slider_of(receiver).max
}

unsafe extern "C" fn set_max_value(receiver: Id, _sel: Sel, value: f64) {
    slider_of(receiver).max = value;
}

unsafe extern "C" fn double_value(receiver: Id, _sel: Sel) -> f64 {
    slider_of(receiver).value
}

/// `-setDoubleValue:`, kept within the range as AppKit keeps it.
unsafe extern "C" fn set_double_value(receiver: Id, _sel: Sel, value: f64) {
    let s = slider_of(receiver);
    s.value = value.max(s.min).min(s.max.max(s.min));
}

unsafe extern "C" fn float_value(receiver: Id, sel: Sel) -> f32 {
    // SAFETY: the same receiver.
    unsafe { double_value(receiver, sel) as f32 }
}

unsafe extern "C" fn set_float_value(receiver: Id, sel: Sel, value: f32) {
    // SAFETY: the same receiver.
    unsafe { set_double_value(receiver, sel, f64::from(value)) }
}

unsafe extern "C" fn int_value(receiver: Id, sel: Sel) -> i32 {
    // SAFETY: the same receiver.
    unsafe { double_value(receiver, sel) as i32 }
}

unsafe extern "C" fn set_int_value(receiver: Id, sel: Sel, value: i32) {
    // SAFETY: the same receiver.
    unsafe { set_double_value(receiver, sel, f64::from(value)) }
}

unsafe extern "C" fn integer_value(receiver: Id, sel: Sel) -> isize {
    // SAFETY: the same receiver.
    unsafe { double_value(receiver, sel) as isize }
}

unsafe extern "C" fn set_integer_value(receiver: Id, sel: Sel, value: isize) {
    // SAFETY: the same receiver.
    unsafe { set_double_value(receiver, sel, value as f64) }
}

unsafe extern "C" fn number_of_tick_marks(receiver: Id, _sel: Sel) -> isize {
    slider_of(receiver).tick_marks
}

unsafe extern "C" fn set_number_of_tick_marks(receiver: Id, _sel: Sel, count: isize) {
    slider_of(receiver).tick_marks = count.max(0);
}

unsafe extern "C" fn tick_mark_position(receiver: Id, _sel: Sel) -> usize {
    slider_of(receiver).tick_mark_position
}

unsafe extern "C" fn set_tick_mark_position(receiver: Id, _sel: Sel, position: usize) {
    slider_of(receiver).tick_mark_position = position;
}

unsafe extern "C" fn vertical(receiver: Id, _sel: Sel) -> u8 {
    u8::from(is_vertical(receiver))
}

unsafe extern "C" fn set_vertical(receiver: Id, _sel: Sel, value: u8) {
    slider_of(receiver).vertical = Some(value != 0);
}

/// `-[NSSlider drawRect:]`: the bar, the tick marks and the knob.
unsafe extern "C" fn draw_rect(receiver: Id, _sel: Sel, _dirty: NSRect) {
    let Some((port, flipped)) = crate::cell::current_port() else {
        return;
    };
    let layout = layout(receiver);
    let Some(p) = aqua::Painter::new(port, layout.frame, flipped) else {
        return;
    };
    let look = look(receiver);
    let fraction = fraction(receiver);
    aqua::slider_bar(&p, layout.bar(), layout.horizontal, fraction, look.disabled);
    let count = slider_of(receiver).tick_marks as usize;
    for tick in (0..count).filter_map(|i| layout.tick(i, count)) {
        aqua::slider_tick(&p, tick);
    }
    aqua::slider_knob(&p, layout.knob(fraction), look);
}

/// The slider a cell reports on.
fn control(cell: Id) -> Id {
    crate::cell::control_of(cell)
}

unsafe extern "C" fn knob_rect_flipped(cell: Id, _sel: Sel, flipped: u8) -> NSRect {
    let slider = control(cell);
    let knob = layout(slider).knob(fraction(slider));
    oriented(slider, knob, flipped != 0)
}

unsafe extern "C" fn bar_rect_flipped(cell: Id, _sel: Sel, flipped: u8) -> NSRect {
    let slider = control(cell);
    oriented(slider, layout(slider).bar(), flipped != 0)
}

unsafe extern "C" fn knob_thickness(cell: Id, _sel: Sel) -> f64 {
    layout(control(cell)).knob_side()
}

/// `-rectOfTickMarkAtIndex:`, in the slider's own coordinates.
unsafe extern "C" fn rect_of_tick_mark_at_index(cell: Id, _sel: Sel, index: isize) -> NSRect {
    let slider = control(cell);
    let count = slider_of(slider).tick_marks;
    let tick = usize::try_from(index)
        .ok()
        .filter(|_| index < count)
        .and_then(|i| layout(slider).tick(i, count as usize));
    let Some(tick) = tick else {
        objc::fail!("NSRangeException: tick mark {index} of a slider with {count}");
    };
    oriented(slider, tick, view::is_flipped(slider))
}

/// `-drawBarInside:flipped:`: the bar, filled to the value, in `rect`.
unsafe extern "C" fn draw_bar_inside(cell: Id, _sel: Sel, rect: NSRect, flipped: u8) {
    let Some((port, _)) = crate::cell::current_port() else {
        return;
    };
    let Some(p) = aqua::Painter::new(port, rect, flipped != 0) else {
        return;
    };
    let slider = control(cell);
    let horizontal = !is_vertical(slider);
    aqua::slider_bar(
        &p,
        rect,
        horizontal,
        fraction(slider),
        look(slider).disabled,
    );
}

/// `-drawKnob:`: the knob, in `rect`.
unsafe extern "C" fn draw_knob_in(cell: Id, _sel: Sel, rect: NSRect) {
    let Some((port, flipped)) = crate::cell::current_port() else {
        return;
    };
    let Some(p) = aqua::Painter::new(port, rect, flipped) else {
        return;
    };
    aqua::slider_knob(&p, rect, look(control(cell)));
}

/// `-drawKnob`: the knob where the value puts it, in the current context.
unsafe extern "C" fn draw_knob(cell: Id, sel: Sel) {
    let Some((_, flipped)) = crate::cell::current_port() else {
        return;
    };
    // SAFETY: the same cell; the rectangle is in the context's coordinates.
    unsafe { draw_knob_in(cell, sel, knob_rect_flipped(cell, sel, u8::from(flipped))) }
}

/// `-startTrackingAt:inView:`: the knob is drawn pressed until tracking stops.
unsafe extern "C" fn start_tracking(cell: Id, _sel: Sel, _at: NSPoint, _view: Id) -> u8 {
    slider_of(control(cell)).tracking = true;
    1
}

unsafe extern "C" fn stop_tracking(
    cell: Id,
    _sel: Sel,
    _last: NSPoint,
    _stop: NSPoint,
    _view: Id,
    _mouse_is_up: u8,
) {
    slider_of(control(cell)).tracking = false;
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSSlider").expect("declared before its methods");
    methods!(class, {
        "initWithFrame:" => init_with_frame, c"@48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "minValue" => min_value, c"d16@0:8";
        "setMinValue:" => set_min_value, c"v24@0:8d16";
        "maxValue" => max_value, c"d16@0:8";
        "setMaxValue:" => set_max_value, c"v24@0:8d16";
        "doubleValue" => double_value, c"d16@0:8";
        "setDoubleValue:" => set_double_value, c"v24@0:8d16";
        "floatValue" => float_value, c"f16@0:8";
        "setFloatValue:" => set_float_value, c"v20@0:8f16";
        "intValue" => int_value, c"i16@0:8";
        "setIntValue:" => set_int_value, c"v20@0:8i16";
        "integerValue" => integer_value, c"q16@0:8";
        "setIntegerValue:" => set_integer_value, c"v24@0:8q16";
        "numberOfTickMarks" => number_of_tick_marks, c"q16@0:8";
        "setNumberOfTickMarks:" => set_number_of_tick_marks, c"v24@0:8q16";
        "tickMarkPosition" => tick_mark_position, c"Q16@0:8";
        "setTickMarkPosition:" => set_tick_mark_position, c"v24@0:8Q16";
        "isVertical" => vertical, c"c16@0:8";
        "setVertical:" => set_vertical, c"v20@0:8c16";
        "drawRect:" => draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
    });
    let cell = objc::class::lookup(c"NSSliderCell").expect("declared before its methods");
    methods!(cell, {
        "knobRectFlipped:" => knob_rect_flipped, c"{CGRect={CGPoint=dd}{CGSize=dd}}20@0:8c16";
        "barRectFlipped:" => bar_rect_flipped, c"{CGRect={CGPoint=dd}{CGSize=dd}}20@0:8c16";
        "knobThickness" => knob_thickness, c"d16@0:8";
        "rectOfTickMarkAtIndex:" => rect_of_tick_mark_at_index,
            c"{CGRect={CGPoint=dd}{CGSize=dd}}24@0:8q16";
        "drawBarInside:flipped:" => draw_bar_inside,
            c"v52@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16c48";
        "drawKnob:" => draw_knob_in, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "drawKnob" => draw_knob, c"v16@0:8";
        "startTrackingAt:inView:" => start_tracking, c"c40@0:8{CGPoint=dd}16@32";
        "stopTracking:at:inView:mouseIsUp:" => stop_tracking,
            c"v60@0:8{CGPoint=dd}16{CGPoint=dd}32@48c56";
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_slider(frame: NSRect) -> Id {
        crate::bootstrap();
        let class = objc::class::lookup(c"NSSlider").expect("NSSlider");
        let slider = objc::object::create_instance(class, 0);
        // SAFETY: `-initWithFrame:` is `id (id, SEL, NSRect)`.
        unsafe { init_with_frame(slider, objc::sel::get("initWithFrame:"), frame) }
    }

    fn send_rect(receiver: Id, selector: &str, flag: u8) -> NSRect {
        // SAFETY: the selectors sent are `NSRect (id, SEL, BOOL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel, u8) -> NSRect =
                std::mem::transmute(objc::msg::entry_stret());
            f(receiver, objc::sel::get(selector), flag)
        }
    }

    #[test]
    fn a_new_slider_spans_zero_to_one() {
        let slider = new_slider(aqua::rect(0.0, 0.0, 100.0, 20.0));
        // SAFETY: the accessors take only the receiver and selector.
        unsafe {
            assert_eq!(min_value(slider, std::ptr::null()), 0.0);
            assert_eq!(max_value(slider, std::ptr::null()), 1.0);
        }
    }

    #[test]
    fn a_slider_value_stays_within_its_range() {
        let slider = new_slider(aqua::rect(0.0, 0.0, 100.0, 20.0));
        let sel = std::ptr::null();
        // SAFETY: the accessors take only the receiver, selector and value.
        unsafe {
            set_max_value(slider, sel, 10.0);
            set_int_value(slider, sel, 12);
            assert_eq!(int_value(slider, sel), 10);
            set_int_value(slider, sel, -3);
            assert_eq!(int_value(slider, sel), 0);
        }
    }

    #[test]
    fn a_slider_cell_answers_the_knob_in_either_orientation() {
        let slider = new_slider(aqua::rect(0.0, 0.0, 100.0, 30.0));
        // SAFETY: the setters take the receiver, selector and value.
        unsafe { set_double_value(slider, std::ptr::null(), 1.0) };
        let cell = crate::cell::cell_of(slider);
        let flipped = send_rect(cell, "knobRectFlipped:", 1);
        let unflipped = send_rect(cell, "knobRectFlipped:", 0);
        assert_eq!(flipped.origin.x, 82.0);
        assert_eq!(flipped.size.width, 18.0);
        assert_eq!(unflipped.origin.x, flipped.origin.x);
        assert_eq!(
            unflipped.origin.y,
            30.0 - flipped.origin.y - flipped.size.height
        );
    }

    #[test]
    fn a_tall_slider_is_vertical_until_told_otherwise() {
        let slider = new_slider(aqua::rect(0.0, 0.0, 20.0, 100.0));
        let sel = std::ptr::null();
        // SAFETY: the accessors take the receiver, selector and value.
        unsafe {
            assert_eq!(vertical(slider, sel), 1);
            set_vertical(slider, sel, 0);
            assert_eq!(vertical(slider, sel), 0);
        }
    }
}
