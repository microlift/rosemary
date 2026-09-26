//! Modal alerts drawn by [`chrome::alert`] in a window of their own, in place
//! of the host toolkit's message box: they look like a Mac's and take a Mac's
//! buttons, which a native box can only approximate.
//!
//! Usable before the rest of AppKit is: nothing here needs a guest.

use std::sync::Mutex;

use chrome::alert::{Alert, AlertLayout, Part};
use chrome::{Canvas, Image, Point, Rect, Theme, View};

/// What to show. The first button is the default.
#[derive(Clone, Debug, Default)]
pub struct Request {
    /// The window's title, for a taskbar; the alert itself shows none.
    pub title: String,
    pub message: String,
    pub informative: String,
    pub buttons: Vec<String>,
    /// The button Escape and closing the window answer. The one titled Cancel
    /// when not given.
    pub cancel: Option<usize>,
    /// Show the application's icon, as `NSAlert` does.
    pub application_icon: bool,
}

/// The button areas of each open dialog's window, for [`hit_test`], which
/// SDL calls with nothing else to go on.
static BUTTONS: Mutex<Vec<(usize, Vec<Rect>)>> = Mutex::new(Vec::new());

unsafe extern "C" fn hit_test(
    window: sdl3::Window,
    point: *const sdl3::video::Point,
    _data: *mut std::ffi::c_void,
) -> sdl3::video::HitTestResult {
    use sdl3::video::HitTestResult as R;
    // SAFETY: SDL passes the point being tested.
    let point = unsafe { *point };
    let point = Point::new(point.x as f32, point.y as f32);
    let dialogs = BUTTONS.lock().unwrap_or_else(|e| e.into_inner());
    let on_button = dialogs
        .iter()
        .find(|(sdl, _)| *sdl == window as usize)
        .is_some_and(|(_, rects)| rects.iter().any(|rect| rect.contains(point)));
    // Everything that is not a button moves the window, as a borderless
    // alert has no titlebar to hold.
    match on_button {
        true => R::Normal,
        false => R::Draggable,
    }
}

fn icon() -> Option<Image> {
    let bitmap = crate::icon::current()?;
    Some(Image {
        width: bitmap.width as usize,
        height: bitmap.height as usize,
        pixels: bitmap.pixels,
    })
}

fn theme() -> Theme {
    Theme::for_appearance(sdl3::video::system_theme() == sdl3::video::SystemTheme::Dark)
}

struct Dialog {
    window: sdl3::Window,
    id: u32,
    alert: Alert,
    layout: AlertLayout,
    theme: Theme,
    /// The button Escape answers.
    cancel: Option<usize>,
    /// The button a press began on, which a release has to end on too.
    armed: Option<usize>,
    /// A modal dialog holds the pointer shown for as long as it is up; a
    /// modeless one only while the pointer is over it, so a game that hid the
    /// pointer keeps it hidden over its own window.
    modal: bool,
    /// The pointer was hidden and this dialog showed it, so closing hides it
    /// again.
    showed_cursor: bool,
    started_video: bool,
}

/// What a dialog made of an event.
enum Outcome {
    Answered(usize),
    Handled,
    /// Not the dialog's, or one every window hears, which the caller passes on.
    Passed,
}

impl Dialog {
    fn paint(&self) {
        let Some((width, _)) = sdl3::video::window_size(self.window) else {
            return;
        };
        sdl3::video::present_window(self.window, |surface| {
            let scale = match width > 0 {
                true => surface.width as f32 / width as f32,
                false => 1.0,
            };
            let length = surface.pitch as usize * surface.height as usize;
            // SAFETY: SDL reported `pitch` bytes per row for `height` rows,
            // and the surface is locked for the duration of this call.
            let pixels = unsafe { std::slice::from_raw_parts_mut(surface.pixels, length) };
            let mut canvas = Canvas::new(
                pixels,
                surface.width as usize,
                surface.height as usize,
                surface.pitch as usize,
                scale,
            );
            canvas.clear(chrome::Color::rgba(0, 0, 0, 0.0));
            self.alert.paint(&mut canvas, &self.layout, &self.theme);
            dump(self.id, surface, pixels);
            true
        });
    }

    fn button_at(&self, x: f32, y: f32) -> Option<usize> {
        self.alert
            .hit(&self.layout, Point::new(x, y))
            .map(|Part::Button(at)| at)
    }

    fn set_pressed(&mut self, pressed: Option<usize>) {
        if self.alert.pressed != pressed {
            self.alert.pressed = pressed;
            self.paint();
        }
    }

    /// Put `request` on screen, or `None` when no dialog could be shown.
    fn open(request: &Request, modal: bool) -> Option<Dialog> {
        if request.buttons.is_empty() || chrome::Font::system(400).is_none() {
            return None;
        }
        crate::decor::close_menus();
        // Hints SDL reads as video starts, which this may be the first to do.
        crate::decor::install();
        let started_video = !sdl3::video::video_up();
        // Before asking for the driver, which SDL cannot name until video is
        // up: the first dialog of a process would otherwise lose its corners.
        if !sdl3::video::init_video() {
            return None;
        }

        let transparent = sdl3::video::is_wayland();
        let alert = Alert {
            message: request.message.clone(),
            informative: request.informative.clone(),
            buttons: request.buttons.clone(),
            icon: match request.application_icon {
                true => icon(),
                false => None,
            },
            pressed: None,
            rounded: transparent,
        };
        let theme = theme();
        let layout = alert.layout(Rect::default(), &theme);
        let cancel = request.cancel.or_else(|| alert.cancel_button());

        let title = std::ffi::CString::new(request.title.replace('\0', " ")).unwrap_or_default();
        let mut flags = sdl3::video::flags::HIDDEN
            | sdl3::video::flags::BORDERLESS
            | sdl3::video::flags::HIGH_PIXEL_DENSITY;
        if transparent {
            flags |= sdl3::video::flags::TRANSPARENT;
        }
        let size = layout.bounds.size();
        let Some(window) = sdl3::video::create_window(
            &title,
            size.width.ceil() as i32,
            size.height.ceil() as i32,
            flags,
        ) else {
            eprintln!(
                "rosetta3: appkit: could not open a dialog: {}",
                sdl3::error()
            );
            if started_video {
                sdl3::video::quit_video();
            }
            return None;
        };
        let parent = parent();
        if modal && !parent.is_null() {
            sdl3::video::set_window_modal_for(window, parent);
        }
        crate::icon::apply(window);
        center(window, parent, size.width, size.height);
        BUTTONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((window as usize, layout.buttons.clone()));
        sdl3::video::set_window_hit_test(window, Some(hit_test));
        sdl3::video::show_window(window);
        sdl3::video::raise_window(window);

        let mut dialog = Dialog {
            window,
            id: sdl3::video::window_id(window),
            alert,
            layout,
            theme,
            cancel,
            armed: None,
            modal,
            showed_cursor: false,
            started_video,
        };
        dialog.paint();
        // A program that hid the pointer, as a game does, would leave nothing
        // to press the buttons with.
        if modal {
            dialog.show_cursor(true);
        }
        Some(dialog)
    }

    /// Show the pointer while `shown`, if the program has it hidden.
    fn show_cursor(&mut self, shown: bool) {
        // SAFETY: reads a counter.
        if unsafe { crate::cursor::CGCursorIsVisible() } || self.showed_cursor == shown {
            return;
        }
        sdl3::video::set_cursor_visible(shown);
        self.showed_cursor = shown;
    }

    /// What closing the window answers: Cancel, or the only button there is.
    fn dismissal(&self) -> Option<usize> {
        self.cancel.or((self.alert.buttons.len() == 1).then_some(0))
    }

    fn handle(&mut self, event: &sdl3::video::Event) -> Outcome {
        use sdl3::keycodes::scancode;
        use sdl3::video::Event;

        let ours = |window: &u32| *window == self.id;
        match event {
            Event::MouseButton {
                window,
                button: sdl3::mouse::LEFT,
                down,
                x,
                y,
                ..
            } if ours(window) => {
                let at = self.button_at(*x, *y);
                if *down {
                    self.armed = at;
                    self.set_pressed(at);
                } else {
                    self.set_pressed(None);
                    if let Some(chosen) = self.armed.take().filter(|&armed| at == Some(armed)) {
                        return Outcome::Answered(chosen);
                    }
                }
            }
            Event::MouseMotion { window, x, y, .. } if ours(window) => {
                if let Some(armed) = self.armed {
                    let over = self.button_at(*x, *y) == Some(armed);
                    self.set_pressed(over.then_some(armed));
                }
            }
            Event::Key {
                window,
                scancode,
                down: true,
                ..
            } if ours(window) => match *scancode {
                scancode::RETURN | scancode::KP_ENTER => return Outcome::Answered(0),
                scancode::ESCAPE => {
                    if let Some(cancel) = self.cancel {
                        return Outcome::Answered(cancel);
                    }
                }
                _ => {}
            },
            Event::WindowClosed { window } if ours(window) => {
                if let Some(answer) = self.dismissal() {
                    return Outcome::Answered(answer);
                }
            }
            Event::WindowResized { window, .. } | Event::WindowBackingChanged { window }
                if ours(window) =>
            {
                self.paint();
            }
            Event::WindowMouse { window, entered } if ours(window) => {
                if !self.modal {
                    self.show_cursor(*entered);
                }
            }
            Event::Key { window, .. }
            | Event::Text { window, .. }
            | Event::TextEditing { window, .. }
            | Event::MouseButton { window, .. }
            | Event::MouseWheel { window, .. }
            | Event::WindowFocus { window, .. }
            | Event::WindowState { window }
            | Event::WindowHitTest { window }
                if ours(window) => {}
            Event::Other(sdl3::video::event_type::WINDOW_EXPOSED) => {
                self.paint();
                return Outcome::Passed;
            }
            Event::Other(sdl3::video::event_type::SYSTEM_THEME_CHANGED) => {
                self.theme = theme();
                self.paint();
                return Outcome::Passed;
            }
            _ => return Outcome::Passed,
        }
        Outcome::Handled
    }

    fn close(mut self) {
        self.show_cursor(false);
        BUTTONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(sdl, _)| *sdl != self.window as usize);
        sdl3::video::set_window_hit_test(self.window, None);
        sdl3::video::destroy_window(self.window);
        if self.started_video {
            sdl3::video::quit_video();
        }
    }
}

/// `ROSETTA3_STORE_OUT=DIR` writes each dialog as it is painted, as the menu
/// panels are, for a session with no screen to look at.
fn dump(id: u32, surface: &sdl3::video::Presentation, pixels: &[u8]) {
    let Some(directory) = std::env::var_os("ROSETTA3_STORE_OUT") else {
        return;
    };
    let (width, height) = (surface.width as usize, surface.height as usize);
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    for row in 0..height {
        let start = row * surface.pitch as usize;
        for texel in pixels[start..start + width * 4].as_chunks::<4>().0 {
            out.extend_from_slice(&[texel[2], texel[1], texel[0]]);
        }
    }
    let _ = std::fs::write(
        std::path::Path::new(&directory).join(format!("dialog-{id}.ppm")),
        out,
    );
}

/// The window a dialog is modal for: the key window, or any of the program's.
fn parent() -> sdl3::Window {
    let key = crate::window::key_window();
    // SAFETY: nil or a window this crate made.
    let sdl = unsafe { crate::window::sdl_window(key) };
    match sdl.is_null() {
        true => crate::window::sdl_windows()
            .into_iter()
            .next()
            .unwrap_or(std::ptr::null_mut()),
        false => sdl,
    }
}

/// Show `request` and wait for a button: its index, or `None` when no dialog
/// could be shown at all, which is the caller's cue to ask some other way.
///
/// Blocks the calling thread, which must be the main one, as `-runModal`
/// does. Input meant for the program's other windows is dropped meanwhile;
/// the rest of what happens to them is still delivered.
pub fn run(request: &Request) -> Option<usize> {
    let mut dialog = Dialog::open(request, true)?;
    let (answer, quit) = modal_loop(&mut dialog);
    let started_video = dialog.started_video;
    dialog.close();
    if quit && !started_video {
        // The quit was the program's, and only postponed.
        crate::event::deliver(sdl3::video::Event::Quit);
    }
    Some(answer)
}

/// A dialog up without a caller waiting on it, and what to do with its answer.
struct Modeless {
    dialog: Dialog,
    answered: Box<dyn FnOnce(usize)>,
}

thread_local! {
    /// The main thread's: dialogs are made and answered there.
    static MODELESS: std::cell::RefCell<Vec<Modeless>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Show `request` and return at once; `answered` is called with the button
/// chosen, from the program's event pump. The window's id, or `None` when no
/// dialog could be shown.
///
/// The program's other windows go on taking input, as an `NSPanel` ordered
/// front rather than run modally does.
pub fn show(request: &Request, answered: impl FnOnce(usize) + 'static) -> Option<u32> {
    let dialog = Dialog::open(request, false)?;
    let id = dialog.id;
    MODELESS.with_borrow_mut(|open| {
        open.push(Modeless {
            dialog,
            answered: Box::new(answered),
        })
    });
    Some(id)
}

/// Bring the modeless dialog `id` back to the front; false when it has been
/// answered since.
pub fn raise(id: u32) -> bool {
    MODELESS.with_borrow(|open| {
        let Some(open) = open.iter().find(|m| m.dialog.id == id) else {
            return false;
        };
        sdl3::video::raise_window(open.dialog.window);
        true
    })
}

/// Offer `event` to the modeless dialogs; true when one took it.
pub(crate) fn deliver(event: &sdl3::video::Event) -> bool {
    let (taken, answered) = MODELESS.with_borrow_mut(|open| {
        for at in 0..open.len() {
            match open[at].dialog.handle(event) {
                Outcome::Answered(answer) => return (true, Some((open.remove(at), answer))),
                Outcome::Handled => return (true, None),
                Outcome::Passed => {}
            }
        }
        (false, None)
    });
    // Out of the borrow: the answer may well open another dialog.
    if let Some((modeless, answer)) = answered {
        modeless.dialog.close();
        (modeless.answered)(answer);
    }
    taken
}

/// Over the parent's middle, or the primary display's. Wayland places a
/// window itself and ignores this.
fn center(window: sdl3::Window, parent: sdl3::Window, width: f32, height: f32) {
    let area = match parent.is_null() {
        false => sdl3::video::window_position(parent).zip(sdl3::video::window_size(parent)),
        true => None,
    }
    .map(|((x, y), (w, h))| (x, y, w, h))
    .or_else(|| {
        let bounds = sdl3::video::display_usable_bounds(sdl3::video::primary_display())?;
        Some((bounds.x, bounds.y, bounds.w, bounds.h))
    });
    if let Some((x, y, w, h)) = area {
        let left = x + (w - width as i32) / 2;
        // A little above the middle, where a Mac puts an alert.
        let top = y + ((h - height as i32) / 3).max(0);
        sdl3::video::set_window_position(window, left, top);
    }
}

/// Turn the event loop until a button is chosen; the answer, and whether a
/// quit arrived meanwhile.
fn modal_loop(dialog: &mut Dialog) -> (usize, bool) {
    use sdl3::video::Event;

    loop {
        // A thread waiting on a GL context the main thread holds would wait
        // for as long as the dialog is up.
        crate::opengl::release_for_other_threads();
        let Some(event) = sdl3::video::wait(50) else {
            continue;
        };
        match dialog.handle(&event) {
            Outcome::Answered(answer) => return (answer, false),
            Outcome::Handled => continue,
            Outcome::Passed => {}
        }
        match event {
            Event::Quit => {
                let answer = dialog.dismissal().unwrap_or(dialog.alert.buttons.len() - 1);
                return (answer, true);
            }
            // The program's windows, and any modeless dialog, take no input
            // while the dialog is up, as under `-runModal`.
            Event::Key { .. }
            | Event::Text { .. }
            | Event::TextEditing { .. }
            | Event::MouseMotion { .. }
            | Event::MouseButton { .. }
            | Event::MouseWheel { .. } => {}
            Event::WindowClosed { .. } | Event::WindowHitTest { .. } => {}
            other => crate::event::deliver(other),
        }
    }
}
