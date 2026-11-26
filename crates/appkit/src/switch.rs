//! `NSSwitch`: a control whose state is on or off, drawn as a toggle that a
//! click flips, sending the control's action.

use foundation::geometry::{NSPoint, NSRect, NSSize};
use objc::object::Id;
use objc::sel::Sel;

use crate::define::methods;
use crate::props;

/// `NSControlStateValueOff` and `NSControlStateValueOn`.
const OFF: isize = 0;
const ON: isize = 1;

/// The regular size's track.
/// FIXME (ACCURACY): from memory of macOS 11+, not measured.
const TRACK: NSSize = NSSize {
    width: 38.0,
    height: 22.0,
};

/// How far the knob sits inside the track.
const KNOB_INSET: f32 = 2.0;

unsafe extern "C" fn state(receiver: Id, _sel: Sel) -> isize {
    props::integer(receiver, "state") as isize
}

unsafe extern "C" fn set_state(receiver: Id, _sel: Sel, state: isize) {
    props::set_integer_of(receiver, "state", state as usize);
    crate::backing::needs_display(receiver);
}

unsafe extern "C" fn intrinsic_content_size(_receiver: Id, _sel: Sel) -> NSSize {
    TRACK
}

fn is_enabled(receiver: Id) -> bool {
    // SAFETY: `BOOL (id, SEL)`, NSControl's.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel) -> u8 = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("isEnabled")) != 0
    }
}

/// `-drawRect:`: the track, filled with the accent colour when on, and the
/// knob at the end the state names.
unsafe extern "C" fn draw_rect(receiver: Id, _sel: Sel, _dirty: NSRect) {
    let bounds = crate::view::bounds_of(receiver);
    let density = crate::backing::focus_density();
    let width = (bounds.size.width * density).ceil() as usize;
    let height = (bounds.size.height * density).ceil() as usize;
    if width == 0 || height == 0 {
        return;
    }
    let mut pixels = vec![0u8; width * height * 4];
    let mut canvas = chrome::Canvas::new(&mut pixels, width, height, width * 4, density as f32);
    let (w, h) = (bounds.size.width as f32, bounds.size.height as f32);
    // The track keeps its proportions, centred in whatever frame it has.
    let track_h = (TRACK.height as f32).min(h);
    let track_w = (TRACK.width as f32).min(w);
    let track = chrome::Rect::new((w - track_w) / 2.0, (h - track_h) / 2.0, track_w, track_h);
    let on = props::integer(receiver, "state") as isize == ON;
    let alpha = if is_enabled(receiver) { 1.0 } else { 0.5 };
    let dark = crate::appearance::is_dark();
    let fill = match on {
        true => {
            let accent = crate::color::named_components("controlAccentColor")
                .unwrap_or((0.0, 0.48, 1.0, 1.0));
            let channel = |c: f64| (c * 255.0).round().clamp(0.0, 255.0) as u8;
            chrome::Color::rgba(
                channel(accent.0),
                channel(accent.1),
                channel(accent.2),
                alpha,
            )
        }
        false if dark => chrome::Color::rgba(255, 255, 255, 0.15 * alpha),
        false => chrome::Color::rgba(0, 0, 0, 0.09 * alpha),
    };
    canvas.fill_rounded_rect(track, track_h / 2.0, fill);
    let radius = track_h / 2.0 - KNOB_INSET;
    let x = match on {
        true => track.x + track_w - KNOB_INSET - radius,
        false => track.x + KNOB_INSET + radius,
    };
    canvas.fill_circle(
        chrome::Point::new(x, track.y + track_h / 2.0),
        radius,
        chrome::Color::rgba(255, 255, 255, alpha),
    );
    crate::backing::draw_image(
        &pixels,
        width,
        height,
        bounds.size,
        bounds,
        NSRect {
            origin: NSPoint::default(),
            size: bounds.size,
        },
        // Source-over.
        2,
        1.0,
        true,
    );
}

/// `-mouseDown:`: a click flips the state and sends the action.
/// FIXME (ACCURACY): no drag of the knob; the press is a click.
unsafe extern "C" fn mouse_down(receiver: Id, _sel: Sel, _event: Id) {
    if !is_enabled(receiver) {
        return;
    }
    let flipped = match props::integer(receiver, "state") as isize {
        OFF => ON,
        _ => OFF,
    };
    // SAFETY: `void (id, SEL, NSInteger)`, sent so observers and overrides see it.
    unsafe {
        let f: unsafe extern "C" fn(Id, Sel, isize) = std::mem::transmute(objc::msg::entry());
        f(receiver, objc::sel::get("setState:"), flipped);
    }
    crate::controls::send_action(receiver);
}

pub(crate) fn install() {
    let class = objc::class::lookup(c"NSSwitch").expect("declared before its methods");
    methods!(class, {
        "state" => state, c"q16@0:8";
        "setState:" => set_state, c"v24@0:8q16";
        "intrinsicContentSize" => intrinsic_content_size, c"{CGSize=dd}16@0:8";
        "drawRect:" => draw_rect, c"v48@0:8{CGRect={CGPoint=dd}{CGSize=dd}}16";
        "mouseDown:" => mouse_down, c"v24@0:8@16";
    });
}
