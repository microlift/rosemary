//! What a guest process and the desktop's host say to each other.
//!
//! A guest asks ([`Request`]) and the host answers each in turn ([`Reply`]),
//! so a guest that has its answer has also seen everything the host sent
//! before it: the new state of its windows ([`Message::Window`]) and the
//! events that came of what it asked ([`Message::Event`]). Reads are then
//! answered from what the guest was last told, without asking.
//!
//! Windows are named by the host's numbers, which no two guests share.

use server::codec::{Decoder, Encoder, Malformed, Wire};

use super::scene::WindowId;
use crate::Rect;
use crate::video::{Event, HitTestResult, PinchPhase};

/// Both ends are this crate; a host of another build is refused.
pub(crate) const VERSION: u32 = 1;

/// `Wire` for an enum of struct-like and unit variants, each under its tag.
macro_rules! wire_enum {
    ($name:ident { $($(#[$attribute:meta])* $tag:literal => $variant:ident $({ $($field:ident),* $(,)? })?),* $(,)? }) => {
        impl Wire for $name {
            fn put(&self, out: &mut Encoder) {
                match self {
                    $(
                        $(#[$attribute])*
                        $name::$variant $({ $($field),* })? => {
                            ($tag as u8).put(out);
                            $($( $field.put(out); )*)?
                        }
                    )*
                }
            }
            fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed> {
                match u8::take(input)? {
                    $(
                        $(#[$attribute])*
                        $tag => Ok($name::$variant $({ $($field: input.take()?),* })?),
                    )*
                    _ => Err(Malformed),
                }
            }
        }
    };
}

/// Where a popup hangs: off `window`, at an offset from its origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Anchor {
    pub window: WindowId,
    pub x: i32,
    pub y: i32,
}
server::wire_struct!(Anchor { window, x, y });

/// What a guest reads back about one of its windows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct State {
    pub frame: Rect,
    /// Where SDL would say it is: from its parent's origin for a popup.
    pub x: i32,
    pub y: i32,
    pub flags: u64,
    pub opacity: f32,
    pub parent: Option<WindowId>,
    /// Whether the host pointer is in relative mode for this window.
    pub relative: bool,
    pub text_input: bool,
}
server::wire_struct!(State {
    frame,
    x,
    y,
    flags,
    opacity,
    parent,
    relative,
    text_input
});

/// The cursor a guest wants shown over its windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CursorChoice {
    /// The host's own arrow.
    #[default]
    Standard,
    /// An `SDL_SystemCursor`.
    System { id: i32 },
    /// One the guest defined with [`Request::DefineCursor`].
    Defined { cursor: u32 },
}
wire_enum!(CursorChoice {
    0 => Standard,
    1 => System { id },
    2 => Defined { cursor },
});

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    /// First on every connection.
    Hello {
        version: u32,
        pid: i32,
    },
    /// A hidden window. Without a `parent` it is centred, as SDL centres one.
    Create {
        width: i32,
        height: i32,
        flags: u64,
        parent: Option<Anchor>,
    },
    Destroy {
        window: WindowId,
    },
    SetSize {
        window: WindowId,
        width: i32,
        height: i32,
    },
    SetPosition {
        window: WindowId,
        x: i32,
        y: i32,
    },
    Show {
        window: WindowId,
    },
    Hide {
        window: WindowId,
    },
    Raise {
        window: WindowId,
        focus: bool,
    },
    /// Enter or leave `state`, which is `FULLSCREEN` or `MAXIMIZED`.
    SetCovering {
        window: WindowId,
        state: u64,
        on: bool,
    },
    SetOpacity {
        window: WindowId,
        opacity: f32,
    },
    SetFlag {
        window: WindowId,
        flag: u64,
        on: bool,
    },
    SetMinimum {
        window: WindowId,
        width: i32,
        height: i32,
    },
    SetModalFor {
        window: WindowId,
        parent: WindowId,
    },
    SetLevel {
        window: WindowId,
        level: i32,
    },
    /// Whether the guest has a hit test for the window, which the host then
    /// asks ([`Message::Probe`]) before it takes a left press as a click.
    SetHitTest {
        window: WindowId,
        on: bool,
    },
    /// The pixels the guest draws the window into: the file `name` in the
    /// desktop's directory, `width * height` pixels of four bytes.
    Attach {
        window: WindowId,
        name: Vec<u8>,
        width: i32,
        height: i32,
    },
    /// The attached pixels are a finished picture, to be shown.
    Commit {
        window: WindowId,
    },
    /// Where the pointer is and what it holds.
    Pointer,
    Warp {
        x: f32,
        y: f32,
    },
    SetRelative {
        window: WindowId,
        on: bool,
    },
    /// A rectangle to keep the pointer in, in the window's coordinates.
    SetMouseRect {
        window: WindowId,
        rect: Option<Rect>,
    },
    SetGrab {
        window: WindowId,
        on: bool,
    },
    StartTextInput {
        window: WindowId,
    },
    StopTextInput {
        window: WindowId,
    },
    SetTextInputArea {
        window: WindowId,
        area: Rect,
        cursor: i32,
    },
    /// `width * height` RGBA pixels, pointing at `(hot_x, hot_y)`.
    DefineCursor {
        cursor: u32,
        width: i32,
        height: i32,
        hot_x: i32,
        hot_y: i32,
        pixels: Vec<u8>,
    },
    ForgetCursor {
        cursor: u32,
    },
    SetCursor {
        choice: CursorChoice,
    },
    SetCursorVisible {
        visible: bool,
    },
    SetClipboard {
        text: Vec<u8>,
    },
    Clipboard,
    /// What the window's hit test made of the point in [`Message::Probe`].
    Probed {
        serial: u32,
        hit: HitTestResult,
    },
    /// Take `event` as though the host's window system had delivered it.
    #[cfg(test)]
    Inject {
        event: Event,
    },
    /// The pixel the host shows at a point of the desktop.
    #[cfg(test)]
    Shown {
        x: i32,
        y: i32,
    },
}
wire_enum!(Request {
    0 => Hello { version, pid },
    1 => Create { width, height, flags, parent },
    2 => Destroy { window },
    3 => SetSize { window, width, height },
    4 => SetPosition { window, x, y },
    5 => Show { window },
    6 => Hide { window },
    7 => Raise { window, focus },
    8 => SetCovering { window, state, on },
    9 => SetOpacity { window, opacity },
    10 => SetFlag { window, flag, on },
    11 => SetMinimum { window, width, height },
    12 => SetModalFor { window, parent },
    13 => SetLevel { window, level },
    14 => SetHitTest { window, on },
    15 => Attach { window, name, width, height },
    16 => Commit { window },
    17 => Pointer,
    18 => Warp { x, y },
    19 => SetRelative { window, on },
    20 => SetMouseRect { window, rect },
    21 => SetGrab { window, on },
    22 => StartTextInput { window },
    23 => StopTextInput { window },
    24 => SetTextInputArea { window, area, cursor },
    25 => DefineCursor { cursor, width, height, hot_x, hot_y, pixels },
    26 => ForgetCursor { cursor },
    27 => SetCursor { choice },
    28 => SetCursorVisible { visible },
    29 => SetClipboard { text },
    30 => Clipboard,
    31 => Probed { serial, hit },
    #[cfg(test)]
    32 => Inject { event },
    #[cfg(test)]
    33 => Shown { x, y },
});

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Reply {
    Welcome,
    Refused {
        why: String,
    },
    Done {
        ok: bool,
    },
    Created {
        window: Option<WindowId>,
    },
    /// The pointer on the desktop, the held buttons, and the asking guest's
    /// window it is over or held by, if it is one of its.
    Pointer {
        x: f32,
        y: f32,
        buttons: u32,
        over: Option<WindowId>,
    },
    Clipboard {
        text: Option<Vec<u8>>,
    },
    #[cfg(test)]
    Pixel {
        blue: u8,
        green: u8,
        red: u8,
        alpha: u8,
    },
}
wire_enum!(Reply {
    0 => Welcome,
    1 => Refused { why },
    2 => Done { ok },
    3 => Created { window },
    4 => Pointer { x, y, buttons, over },
    5 => Clipboard { text },
    #[cfg(test)]
    6 => Pixel { blue, green, red, alpha },
});

impl Request {
    /// The window of the asking guest's the request leans on, which the host
    /// checks is that guest's before anything else.
    pub(crate) fn window(&self) -> Option<WindowId> {
        match self {
            Request::Create { parent, .. } => parent.map(|anchor| anchor.window),
            Request::Destroy { window }
            | Request::SetSize { window, .. }
            | Request::SetPosition { window, .. }
            | Request::Show { window }
            | Request::Hide { window }
            | Request::Raise { window, .. }
            | Request::SetCovering { window, .. }
            | Request::SetOpacity { window, .. }
            | Request::SetFlag { window, .. }
            | Request::SetMinimum { window, .. }
            | Request::SetModalFor { window, .. }
            | Request::SetLevel { window, .. }
            | Request::SetHitTest { window, .. }
            | Request::Attach { window, .. }
            | Request::Commit { window }
            | Request::SetRelative { window, .. }
            | Request::SetMouseRect { window, .. }
            | Request::SetGrab { window, .. }
            | Request::StartTextInput { window }
            | Request::StopTextInput { window }
            | Request::SetTextInputArea { window, .. } => Some(*window),
            Request::Hello { .. }
            | Request::Pointer
            | Request::Warp { .. }
            | Request::DefineCursor { .. }
            | Request::ForgetCursor { .. }
            | Request::SetCursor { .. }
            | Request::SetCursorVisible { .. }
            | Request::SetClipboard { .. }
            | Request::Clipboard
            | Request::Probed { .. } => None,
            #[cfg(test)]
            Request::Inject { .. } | Request::Shown { .. } => None,
        }
    }
}

/// What the host sends a guest.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Message {
    Reply {
        reply: Reply,
    },
    /// The desktop's size in points, and host pixels per point.
    Desktop {
        width: i32,
        height: i32,
        density: f32,
    },
    Window {
        window: WindowId,
        state: State,
    },
    /// Which modifier keys are held, as `SDL_Keymod` bits.
    Modifiers {
        modifiers: u16,
    },
    Event {
        event: Event,
    },
    /// A left press is about to land on `window` at this point of it: run
    /// the window's hit test and answer with [`Request::Probed`].
    Probe {
        serial: u32,
        window: WindowId,
        x: i32,
        y: i32,
    },
}
wire_enum!(Message {
    0 => Reply { reply },
    1 => Desktop { width, height, density },
    2 => Window { window, state },
    3 => Modifiers { modifiers },
    4 => Event { event },
    5 => Probe { serial, window, x, y },
});

impl Wire for Rect {
    fn put(&self, out: &mut Encoder) {
        out.put(&self.x).put(&self.y).put(&self.w).put(&self.h);
    }
    fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed> {
        Ok(Rect {
            x: input.take()?,
            y: input.take()?,
            w: input.take()?,
            h: input.take()?,
        })
    }
}

impl Wire for HitTestResult {
    fn put(&self, out: &mut Encoder) {
        (*self as u8).put(out);
    }
    fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed> {
        use HitTestResult::*;
        let raw = u8::take(input)?;
        [
            Normal,
            Draggable,
            ResizeTopLeft,
            ResizeTop,
            ResizeTopRight,
            ResizeRight,
            ResizeBottomRight,
            ResizeBottom,
            ResizeBottomLeft,
            ResizeLeft,
        ]
        .into_iter()
        .find(|hit| *hit as u8 == raw)
        .ok_or(Malformed)
    }
}

wire_enum!(PinchPhase {
    0 => Began,
    1 => Changed,
    2 => Ended,
});

/// `Event::Other` is SDL's own business and never crosses.
const EVENT_OTHER: u8 = u8::MAX;

impl Wire for Event {
    fn put(&self, out: &mut Encoder) {
        match self {
            Event::Quit => {
                out.put(&0u8);
            }
            Event::WindowMoved { window } => {
                out.put(&1u8).put(window);
            }
            Event::WindowDisplayChanged { window } => {
                out.put(&2u8).put(window);
            }
            Event::DisplayModeChanged { display } => {
                out.put(&3u8).put(display);
            }
            Event::WindowResized {
                window,
                width,
                height,
            } => {
                out.put(&4u8).put(window).put(width).put(height);
            }
            Event::WindowBackingChanged { window } => {
                out.put(&5u8).put(window);
            }
            Event::WindowClosed { window } => {
                out.put(&6u8).put(window);
            }
            Event::WindowFocus { window, gained } => {
                out.put(&7u8).put(window).put(gained);
            }
            Event::WindowMouse { window, entered } => {
                out.put(&8u8).put(window).put(entered);
            }
            Event::WindowHitTest { window } => {
                out.put(&9u8).put(window);
            }
            Event::WindowState { window } => {
                out.put(&10u8).put(window);
            }
            Event::Key {
                window,
                scancode,
                key,
                modifiers,
                down,
                repeat,
            } => {
                out.put(&11u8)
                    .put(window)
                    .put(scancode)
                    .put(key)
                    .put(modifiers)
                    .put(down)
                    .put(repeat);
            }
            Event::Text { window, text } => {
                out.put(&12u8).put(window).put(text);
            }
            Event::TextEditing {
                window,
                text,
                start,
                length,
            } => {
                out.put(&13u8).put(window).put(text).put(start).put(length);
            }
            Event::MouseMotion {
                window,
                x,
                y,
                dx,
                dy,
                buttons,
            } => {
                out.put(&14u8)
                    .put(window)
                    .put(x)
                    .put(y)
                    .put(dx)
                    .put(dy)
                    .put(buttons);
            }
            Event::MouseButton {
                window,
                button,
                down,
                clicks,
                x,
                y,
            } => {
                out.put(&15u8)
                    .put(window)
                    .put(button)
                    .put(down)
                    .put(clicks)
                    .put(x)
                    .put(y);
            }
            Event::MouseWheel { window, x, y } => {
                out.put(&16u8).put(window).put(x).put(y);
            }
            Event::Pinch {
                window,
                scale,
                phase,
            } => {
                out.put(&17u8).put(window).put(scale).put(phase);
            }
            Event::Other(kind) => {
                out.put(&EVENT_OTHER).put(kind);
            }
        }
    }

    fn take(input: &mut Decoder<'_>) -> Result<Self, Malformed> {
        Ok(match u8::take(input)? {
            0 => Event::Quit,
            1 => Event::WindowMoved {
                window: input.take()?,
            },
            2 => Event::WindowDisplayChanged {
                window: input.take()?,
            },
            3 => Event::DisplayModeChanged {
                display: input.take()?,
            },
            4 => Event::WindowResized {
                window: input.take()?,
                width: input.take()?,
                height: input.take()?,
            },
            5 => Event::WindowBackingChanged {
                window: input.take()?,
            },
            6 => Event::WindowClosed {
                window: input.take()?,
            },
            7 => Event::WindowFocus {
                window: input.take()?,
                gained: input.take()?,
            },
            8 => Event::WindowMouse {
                window: input.take()?,
                entered: input.take()?,
            },
            9 => Event::WindowHitTest {
                window: input.take()?,
            },
            10 => Event::WindowState {
                window: input.take()?,
            },
            11 => Event::Key {
                window: input.take()?,
                scancode: input.take()?,
                key: input.take()?,
                modifiers: input.take()?,
                down: input.take()?,
                repeat: input.take()?,
            },
            12 => Event::Text {
                window: input.take()?,
                text: input.take()?,
            },
            13 => Event::TextEditing {
                window: input.take()?,
                text: input.take()?,
                start: input.take()?,
                length: input.take()?,
            },
            14 => Event::MouseMotion {
                window: input.take()?,
                x: input.take()?,
                y: input.take()?,
                dx: input.take()?,
                dy: input.take()?,
                buttons: input.take()?,
            },
            15 => Event::MouseButton {
                window: input.take()?,
                button: input.take()?,
                down: input.take()?,
                clicks: input.take()?,
                x: input.take()?,
                y: input.take()?,
            },
            16 => Event::MouseWheel {
                window: input.take()?,
                x: input.take()?,
                y: input.take()?,
            },
            17 => Event::Pinch {
                window: input.take()?,
                scale: input.take()?,
                phase: input.take()?,
            },
            EVENT_OTHER => Event::Other(input.take()?),
            _ => return Err(Malformed),
        })
    }
}

const FRAME_HEADER: usize = size_of::<u32>();

/// Add `value` to `out` as one length-prefixed frame, which is what
/// `server::codec::write_frame` writes.
pub(crate) fn put_frame<T: Wire>(out: &mut Vec<u8>, value: &T) {
    let payload = server::codec::encode(value);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
}

/// A whole frame off the front of `unread`, if all of one has arrived.
pub(crate) fn take_frame(unread: &mut Vec<u8>) -> Result<Option<Vec<u8>>, Malformed> {
    let Some(header) = unread.first_chunk::<FRAME_HEADER>() else {
        return Ok(None);
    };
    let len = u32::from_le_bytes(*header) as usize;
    if len > server::codec::MAX_FRAME {
        return Err(Malformed);
    }
    if unread.len() < FRAME_HEADER + len {
        return Ok(None);
    }
    let frame = unread[FRAME_HEADER..FRAME_HEADER + len].to_vec();
    unread.drain(..FRAME_HEADER + len);
    Ok(Some(frame))
}

/// The window an event is about, where it is about one.
pub(crate) fn window_of(event: &Event) -> Option<WindowId> {
    match event {
        Event::WindowMoved { window }
        | Event::WindowDisplayChanged { window }
        | Event::WindowResized { window, .. }
        | Event::WindowBackingChanged { window }
        | Event::WindowClosed { window }
        | Event::WindowFocus { window, .. }
        | Event::WindowMouse { window, .. }
        | Event::WindowHitTest { window }
        | Event::WindowState { window }
        | Event::Key { window, .. }
        | Event::Text { window, .. }
        | Event::TextEditing { window, .. }
        | Event::MouseMotion { window, .. }
        | Event::MouseButton { window, .. }
        | Event::MouseWheel { window, .. }
        | Event::Pinch { window, .. } => Some(*window),
        Event::Quit | Event::DisplayModeChanged { .. } | Event::Other(_) => None,
    }
}

/// `event` about `window` instead of the one it names.
pub(crate) fn about(mut event: Event, to: WindowId) -> Event {
    match &mut event {
        Event::WindowMoved { window }
        | Event::WindowDisplayChanged { window }
        | Event::WindowResized { window, .. }
        | Event::WindowBackingChanged { window }
        | Event::WindowClosed { window }
        | Event::WindowFocus { window, .. }
        | Event::WindowMouse { window, .. }
        | Event::WindowHitTest { window }
        | Event::WindowState { window }
        | Event::Key { window, .. }
        | Event::Text { window, .. }
        | Event::TextEditing { window, .. }
        | Event::MouseMotion { window, .. }
        | Event::MouseButton { window, .. }
        | Event::MouseWheel { window, .. }
        | Event::Pinch { window, .. } => *window = to,
        Event::Quit | Event::DisplayModeChanged { .. } | Event::Other(_) => {}
    }
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use server::codec::{decode, encode};

    #[test]
    fn a_request_with_every_kind_of_field_round_trips() {
        let requests = [
            Request::Create {
                width: 640,
                height: 480,
                flags: 1 << 40,
                parent: Some(Anchor {
                    window: 7,
                    x: -3,
                    y: 12,
                }),
            },
            Request::SetMouseRect {
                window: 2,
                rect: Some(Rect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4,
                }),
            },
            Request::SetCursor {
                choice: CursorChoice::Defined { cursor: 9 },
            },
            Request::Probed {
                serial: 5,
                hit: HitTestResult::ResizeBottomLeft,
            },
            Request::Attach {
                window: 3,
                name: b"buffer-1-2".to_vec(),
                width: 10,
                height: 20,
            },
            Request::Pointer,
        ];
        for request in requests {
            assert_eq!(decode::<Request>(&encode(&request)), Ok(request));
        }
    }

    #[test]
    fn an_event_keeps_its_text_and_fractions_across_the_socket() {
        let events = [
            Event::TextEditing {
                window: 4,
                text: "\u{65e5}\u{672c}".into(),
                start: 1,
                length: 2,
            },
            Event::MouseMotion {
                window: 4,
                x: 10.5,
                y: -0.25,
                dx: 1.0,
                dy: 0.0,
                buttons: 5,
            },
            Event::Pinch {
                window: 1,
                scale: 1.5,
                phase: PinchPhase::Changed,
            },
            Event::Quit,
        ];
        for event in events {
            let message = Message::Event { event };
            assert_eq!(decode::<Message>(&encode(&message)), Ok(message));
        }
    }

    #[test]
    fn frames_come_off_a_stream_whole_however_it_was_cut() {
        let mut stream = Vec::new();
        put_frame(&mut stream, &Request::Pointer);
        put_frame(&mut stream, &Request::Show { window: 3 });
        let mut unread = Vec::new();
        let mut frames = Vec::new();
        for byte in stream {
            unread.push(byte);
            while let Some(frame) = take_frame(&mut unread).unwrap() {
                frames.push(decode::<Request>(&frame).unwrap());
            }
        }
        assert_eq!(frames, [Request::Pointer, Request::Show { window: 3 }]);
        assert!(unread.is_empty());
    }

    #[test]
    fn a_tag_nobody_sends_is_refused() {
        assert_eq!(decode::<Reply>(&[200]), Err(Malformed));
        assert_eq!(decode::<HitTestResult>(&[10]), Err(Malformed));
    }

    #[test]
    fn an_event_is_readdressed_without_losing_what_it_says() {
        let event = Event::WindowResized {
            window: 1,
            width: 30,
            height: 40,
        };
        let moved = about(event, 9);
        assert_eq!(window_of(&moved), Some(9));
        assert_eq!(
            moved,
            Event::WindowResized {
                window: 9,
                width: 30,
                height: 40
            }
        );
    }
}
