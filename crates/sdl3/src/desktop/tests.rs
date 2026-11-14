//! A guest and a host, each its own process as in a real session, on SDL's
//! dummy video driver.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::client::{Connection, ask};
use super::compose::BACKGROUND;
use super::protocol::{Message, Reply, Request};
use super::shared::Shared;
use super::{Config, ENV, Extent, host, socket_in};
use crate::video::{self, Event, HitTestResult, Point, flags};
use crate::{Rect, Window};

/// Set for the copy of this test binary that is the desktop's host, to the
/// directory it listens in.
const HOST: &str = "ROSEMARY_DESKTOP_TEST_HOST";

const RED: [u8; 4] = [0, 0, 0xff, 0xff];
const BLUE: [u8; 4] = [0xff, 0, 0, 0xff];

fn directory() -> PathBuf {
    std::env::temp_dir().join(format!("rosemary-desktop-test-{}", std::process::id()))
}

/// The desktop a test's guest starts: this binary again, running only
/// [`the_host_serves_until_its_guests_have_gone`].
fn launch(directory: &Path, extent: Extent) -> std::io::Result<()> {
    std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "desktop::tests::the_host_serves_until_its_guests_have_gone",
            "--test-threads=1",
        ])
        .env(HOST, directory)
        .env(ENV, extent.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

/// Not a test of its own: the host half of every test below.
#[test]
fn the_host_serves_until_its_guests_have_gone() {
    let Some(directory) = std::env::var_os(HOST) else {
        return;
    };
    let extent = std::env::var(ENV)
        .expect("an extent")
        .parse()
        .expect("a valid one");
    let directory = PathBuf::from(directory);
    host::run(&host::Options {
        directory: directory.clone(),
        extent,
        linger: Duration::from_millis(200),
    })
    .expect("a desktop");
    // Its guests are gone, and nobody else was told where it is.
    let _ = std::fs::remove_dir_all(directory);
}

/// Whether the calling test should run here: alone in its process, as a
/// guest pointed at a desktop of its own.
fn on_a_desktop() -> bool {
    if !crate::isolated::run_here_on_dummy_desktop("320x200") {
        return false;
    }
    super::configure(Config {
        directory: directory(),
        launch: Some(launch),
    });
    true
}

fn window(at: (i32, i32), colour: [u8; 4]) -> Window {
    let window = video::create_window(c"window", 100, 50, flags::HIDDEN).expect("a window");
    assert!(video::set_window_position(window, at.0, at.1));
    assert!(video::present_window(window, |surface| {
        // SAFETY: the buffer the desktop lent for the length of the call.
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(
                surface.pixels,
                surface.pitch as usize * surface.height as usize,
            )
        };
        for pixel in pixels.as_chunks_mut::<4>().0 {
            *pixel = colour;
        }
        true
    }));
    assert!(video::show_window(window));
    window
}

/// What the host window shows at a desktop point.
fn shown_at(x: i32, y: i32) -> [u8; 4] {
    match ask(Request::Shown { x, y }) {
        Some(Reply::Pixel {
            blue,
            green,
            red,
            alpha,
        }) => [blue, green, red, alpha],
        other => panic!("no pixel: {other:?}"),
    }
}

fn inject(event: Event) {
    assert_eq!(
        ask(Request::Inject { event }),
        Some(Reply::Done { ok: true })
    );
}

fn events() -> Vec<Event> {
    std::iter::from_fn(video::poll).collect()
}

/// Events about the host window, which the host readdresses to itself.
const HOST_WINDOW: u32 = 0;

fn motion(x: f32, y: f32) -> Event {
    Event::MouseMotion {
        window: HOST_WINDOW,
        x,
        y,
        dx: 0.0,
        dy: 0.0,
        buttons: 0,
    }
}

fn button(down: bool, x: f32, y: f32) -> Event {
    Event::MouseButton {
        window: HOST_WINDOW,
        button: crate::mouse::LEFT,
        down,
        clicks: 1,
        x,
        y,
    }
}

/// The dummy driver gives no window the keyboard by itself.
fn focus_the_host() {
    inject(Event::WindowFocus {
        window: HOST_WINDOW,
        gained: true,
    });
    events();
}

#[test]
fn windows_share_one_host_window_and_stack_in_it() {
    if !on_a_desktop() {
        return;
    }
    let back = window((10, 10), RED);
    let front = window((60, 30), BLUE);
    assert_eq!(
        video::display_bounds(video::primary_display()),
        Some(Rect {
            x: 0,
            y: 0,
            w: 320,
            h: 200
        })
    );
    assert_eq!(video::window_position(front), Some((60, 30)));
    assert_eq!(video::window_flags(front) & flags::HIDDEN, 0);
    assert_eq!(shown_at(15, 15), RED);
    assert_eq!(shown_at(70, 40), BLUE);
    assert_eq!(shown_at(5, 5), BACKGROUND);

    assert!(video::raise_window(back));
    assert_eq!(shown_at(70, 40), RED);

    assert!(video::set_window_position(back, 200, 100));
    assert_eq!(shown_at(70, 40), BLUE);
    assert_eq!(shown_at(15, 15), BACKGROUND);
    assert_eq!(shown_at(210, 110), RED);

    video::destroy_window(front);
    assert_eq!(shown_at(70, 40), BACKGROUND);
}

unsafe extern "C" fn everywhere_draggable(
    _window: Window,
    _point: *const Point,
    _data: *mut std::ffi::c_void,
) -> HitTestResult {
    HitTestResult::Draggable
}

#[test]
fn host_input_reaches_the_window_under_the_pointer() {
    if !on_a_desktop() {
        return;
    }
    let plain = window((10, 10), RED);
    let titled = window((150, 100), BLUE);
    assert!(video::set_window_hit_test(
        titled,
        Some(everywhere_draggable)
    ));
    focus_the_host();

    inject(motion(20.0, 15.0));
    inject(button(true, 20.0, 15.0));
    inject(button(false, 20.0, 15.0));
    let id = video::window_id(plain);
    assert!(events().contains(&Event::MouseButton {
        window: id,
        button: 1,
        down: true,
        clicks: 1,
        x: 10.0,
        y: 5.0,
    }));
    assert_eq!(video::mouse_state(), Some((10.0, 5.0, 0)));
    assert_eq!(video::global_mouse_state(), Some((20.0, 15.0, 0)));
    assert_ne!(video::window_flags(plain) & flags::INPUT_FOCUS, 0);

    // A press its hit test calls draggable moves the window instead. The
    // press waits in the host for the hit test, which polling runs.
    inject(button(true, 160.0, 110.0));
    inject(motion(170.0, 130.0));
    inject(button(false, 170.0, 130.0));
    let mut seen = events();
    assert_eq!(ask(Request::Pointer).map(|_| ()), Some(()));
    seen.extend(events());
    assert_eq!(video::window_position(titled), Some((160, 120)));
    assert!(seen.contains(&Event::WindowMoved {
        window: video::window_id(titled)
    }));

    // Closing the desktop closes the session.
    inject(Event::WindowClosed {
        window: HOST_WINDOW,
    });
    assert!(events().contains(&Event::Quit));
}

#[test]
fn what_the_desktop_cannot_show_is_refused() {
    if !on_a_desktop() {
        return;
    }
    assert!(video::create_window(c"gl", 10, 10, flags::HIDDEN | flags::OPENGL).is_none());
    let plain = video::create_window(c"plain", 10, 10, flags::HIDDEN).expect("a window");
    assert!(video::gl_create_context(plain).is_none());
    assert!(!video::minimize_window(plain));
}

/// Another guest of the same desktop, as a process of its own would be:
/// a connection, and what the host has sent down it.
struct Guest {
    connection: Connection,
    heard: Vec<Message>,
}

impl Guest {
    fn join() -> Guest {
        let mut heard = Vec::new();
        let connection = Connection::open(&socket_in(&directory()), &mut |message| {
            heard.push(message);
        })
        .expect("a second guest");
        Guest { connection, heard }
    }

    fn ask(&mut self, request: Request) -> Reply {
        let heard = &mut self.heard;
        self.connection
            .call(&request, &mut |message| heard.push(message))
            .expect("an answer")
    }

    /// A shown window of one colour.
    fn window(&mut self, at: (i32, i32), colour: [u8; 4]) -> u32 {
        let (width, height) = (100, 50);
        let created = self.ask(Request::Create {
            width,
            height,
            flags: flags::HIDDEN,
            parent: None,
        });
        let Reply::Created {
            window: Some(window),
        } = created
        else {
            panic!("no window: {created:?}");
        };
        let (mut buffer, name) = Shared::create(&directory(), width, height).expect("pixels");
        for pixel in buffer.pixels_mut().as_chunks_mut::<4>().0 {
            *pixel = colour;
        }
        let done = Reply::Done { ok: true };
        let attach = Request::Attach {
            window,
            name: name.clone().into_os_string().into_encoded_bytes(),
            width,
            height,
        };
        assert_eq!(self.ask(attach), done);
        std::fs::remove_file(directory().join(name)).expect("the buffer's file");
        let (x, y) = at;
        assert_eq!(self.ask(Request::SetPosition { window, x, y }), done);
        assert_eq!(self.ask(Request::Commit { window }), done);
        assert_eq!(self.ask(Request::Show { window }), done);
        window
    }

    fn events(&mut self) -> Vec<Event> {
        // An answer comes after everything sent before it.
        self.ask(Request::Pointer);
        std::mem::take(&mut self.heard)
            .into_iter()
            .filter_map(|message| match message {
                Message::Event { event } => Some(event),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn two_guests_share_one_desktop_and_each_gets_its_own_input() {
    if !on_a_desktop() {
        return;
    }
    let ours = window((10, 10), RED);
    let mut other = Guest::join();
    let theirs = other.window((60, 30), BLUE);
    // One stacking order: the later window covers the earlier, whoever made
    // either, and the earlier comes back over it when raised.
    assert_eq!(shown_at(15, 15), RED);
    assert_eq!(shown_at(70, 40), BLUE);
    assert!(video::raise_window(ours));
    assert_eq!(shown_at(70, 40), RED);
    assert_ne!(video::window_flags(ours) & flags::INPUT_FOCUS, 0);
    focus_the_host();
    other.events();

    // A click on the other guest's window is that guest's alone, and takes
    // the keyboard from this one's.
    inject(motion(150.0, 70.0));
    inject(button(true, 150.0, 70.0));
    inject(button(false, 150.0, 70.0));
    let pressed = Event::MouseButton {
        window: theirs,
        button: crate::mouse::LEFT,
        down: true,
        clicks: 1,
        x: 90.0,
        y: 40.0,
    };
    assert!(other.events().contains(&pressed));
    let seen = events();
    assert!(seen.contains(&Event::WindowFocus {
        window: video::window_id(ours),
        gained: false
    }));
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event, Event::MouseButton { .. }))
    );
    assert_eq!(video::window_flags(ours) & flags::INPUT_FOCUS, 0);
    assert_eq!(shown_at(70, 40), BLUE);

    // One guest cannot speak for another's window.
    let (x, y) = (0, 0);
    let window = theirs + 1000;
    assert_eq!(
        other.ask(Request::SetPosition { window, x, y }),
        Reply::Done { ok: false }
    );
}

#[test]
fn a_guest_that_leaves_takes_its_windows_with_it() {
    if !on_a_desktop() {
        return;
    }
    window((200, 120), RED);
    let mut other = Guest::join();
    other.window((10, 10), BLUE);
    assert_eq!(shown_at(15, 15), BLUE);
    drop(other);
    let deadline = Instant::now() + Duration::from_secs(5);
    while shown_at(15, 15) != BACKGROUND {
        assert!(Instant::now() < deadline, "the window outlived its guest");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(shown_at(210, 130), RED);
}

#[test]
fn the_extent_is_fullscreen_or_a_size() {
    let parse = |value: &str| value.parse::<Extent>().ok();
    assert_eq!(parse("fullscreen"), Some(Extent::Fullscreen));
    assert_eq!(parse(" Fullscreen "), Some(Extent::Fullscreen));
    assert_eq!(parse("1280x800"), Some(Extent::Sized(1280, 800)));
    assert_eq!(parse("1280X800"), Some(Extent::Sized(1280, 800)));
    assert_eq!(parse("0x800"), None);
    assert_eq!(parse("1280"), None);
    assert_eq!(parse("on"), None);
    assert_eq!(Extent::Sized(1280, 800).to_string(), "1280x800");
}
