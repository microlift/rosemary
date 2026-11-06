use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use sdl3::video::Event;

pub const ENV: &str = "ROSEMARY_DEBUG_INPUT";
pub const FILE_ENV: &str = "ROSEMARY_DEBUG_INPUT_FILE";
pub const WINDOW_ENV: &str = "ROSEMARY_DEBUG_INPUT_WINDOW";

/// Where the script last put the pointer, in SDL window coordinates, and
/// whether it has put it anywhere at all.
static POINTER: Mutex<(f32, f32)> = Mutex::new((0.0, 0.0));
static MOVED: AtomicBool = AtomicBool::new(false);

/// The pointer position the script last set, on the desktop, for callers that
/// ask the window system where the cursor is rather than reading it off an
/// event. `None` while no script has moved it, so a person's cursor is still
/// the answer in every ordinary run.
pub fn pointer() -> Option<(f32, f32)> {
    if !MOVED.load(Ordering::Relaxed) {
        return None;
    }
    let (x, y) = *POINTER.lock().unwrap_or_else(|e| e.into_inner());
    // Against the window the script is driving, not the first one made: a step
    // is written in the coordinates of the window it names, and measuring it
    // from another window puts the pointer somewhere neither of them is - which
    // reads exactly like the program ignoring the pointer, since every tracking
    // area then decides it is somewhere else.
    let origin = target_window()
        .and_then(crate::window::global_position)
        .unwrap_or((0, 0));
    Some((x + origin.0 as f32, y + origin.1 as f32))
}

#[derive(Debug, PartialEq)]
enum Action {
    Move(f32, f32),
    /// Press and release, `clicks` times, at the current position.
    Click {
        button: u8,
        clicks: i64,
    },
    Down(u8),
    Up(u8),
    Scroll(f32, f32),
    /// One key, by the name [`crate::event::scancode`] knows or the
    /// character it types, with `ctrl+`, `shift+`, `alt+` or `cmd+` held.
    Key {
        scancode: u32,
        character: u32,
        modifiers: u16,
    },
    /// One key pressed or released alone, for a program that acts on a key
    /// for as long as it is held - walking, in a game.
    Hold {
        scancode: u32,
        character: u32,
        modifiers: u16,
        down: bool,
    },
    /// Each character in turn, as the key that types it.
    Type(String),
    /// Click the notification-area entry with this label, which no window
    /// event can reach.
    Tray(String),
    /// Drive a different window from here on, by the names
    /// `ROSEMARY_DEBUG_INPUT_WINDOW` takes.
    ///
    /// A menu is opened by a click on one window and answers clicks on another,
    /// so which window a script drives cannot be settled before it runs.
    Window(String),
}

struct Step {
    at: f64,
    action: Action,
}

/// Parse `SECONDS:ACTION[;SECONDS:ACTION...]`.
///
/// A step nothing understands is named on stderr rather than dropped: a typo in
/// a script that silently does nothing looks exactly like a program that ignored
/// the event, which is the question the script was written to answer.
fn parse(value: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    for piece in value.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((at, rest)) = piece.split_once(':') else {
            eprintln!("rosemary: appkit: {ENV}: {piece:?} has no SECONDS: prefix");
            continue;
        };
        let Ok(at) = at.trim().parse::<f64>() else {
            eprintln!("rosemary: appkit: {ENV}: {at:?} is not a number of seconds");
            continue;
        };
        match action(rest.trim()) {
            Some(action) => steps.push(Step { at, action }),
            None => eprintln!("rosemary: appkit: {ENV}: {rest:?} is not an action"),
        }
    }
    steps.sort_by(|a, b| a.at.total_cmp(&b.at));
    steps
}

/// One `VERB [ARGUMENT]`, the body of a script step.
fn action(rest: &str) -> Option<Action> {
    let (verb, argument) = rest.split_once(' ').unwrap_or((rest, ""));
    let argument = argument.trim();
    match verb {
        "move" => point(argument).map(|(x, y)| Action::Move(x, y)),
        "click" => Some(Action::Click {
            button: argument.parse().unwrap_or(1),
            clicks: 1,
        }),
        "doubleclick" => Some(Action::Click {
            button: argument.parse().unwrap_or(1),
            clicks: 2,
        }),
        "down" => Some(Action::Down(argument.parse().unwrap_or(1))),
        "up" => Some(Action::Up(argument.parse().unwrap_or(1))),
        "scroll" => point(argument).map(|(x, y)| Action::Scroll(x, y)),
        "key" => chord(argument),
        "keydown" | "keyup" => match chord(argument)? {
            Action::Key {
                scancode,
                character,
                modifiers,
            } => Some(Action::Hold {
                scancode,
                character,
                modifiers,
                down: verb == "keydown",
            }),
            _ => None,
        },
        "type" => Some(Action::Type(argument.to_owned())),
        "window" => Some(Action::Window(argument.to_owned())),
        "tray" => Some(Action::Tray(argument.to_owned())),
        _ => None,
    }
}

/// `[MODIFIER+]...KEY`, e.g. `ctrl+q` or `escape`.
fn chord(argument: &str) -> Option<Action> {
    let mut parts: Vec<&str> = argument.split('+').map(str::trim).collect();
    let name = parts.pop()?;
    let mut modifiers = 0u16;
    for part in parts {
        // `SDL_KMOD_L*`, which `-modifierFlags` reports as the left key.
        modifiers |= match part.to_ascii_lowercase().as_str() {
            "shift" => 0x0001,
            "ctrl" | "control" => 0x0040,
            "alt" | "option" => 0x0100,
            "cmd" | "command" | "gui" | "super" => 0x0400,
            _ => return None,
        };
    }
    let mut chars = name.chars();
    let (scancode, character) = match (chars.next(), chars.next()) {
        (Some(c), None) => (
            typed(c.to_ascii_lowercase())?.0,
            c.to_ascii_lowercase() as u32,
        ),
        _ => (crate::event::scancode(name)?, 0),
    };
    Some(Action::Key {
        scancode,
        character,
        modifiers,
    })
}

fn point(argument: &str) -> Option<(f32, f32)> {
    let (x, y) = argument.split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Perform whatever has been appended to `ROSEMARY_DEBUG_INPUT_FILE`.
///
/// A timed script has to be written before the run begins, which is exactly
/// backwards for finding one's way through a program's own interface: where the
/// next thing to click is depends on what the last click did. This reads the
/// file's new lines each pump and performs each as one step, so a session can
/// be driven a line at a time from outside while it runs.
fn run_file() {
    use std::io::Read;

    static PATH: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    static READ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static PARTIAL: Mutex<String> = Mutex::new(String::new());

    let Some(path) = PATH
        .get_or_init(|| std::env::var_os(FILE_ENV).map(std::path::PathBuf::from))
        .as_ref()
    else {
        return;
    };
    let from = READ.load(Ordering::Relaxed);
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    if std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(from)).is_err() {
        return;
    }
    let mut fresh = String::new();
    let Ok(read) = file.read_to_string(&mut fresh) else {
        return;
    };
    READ.store(from + read as u64, Ordering::Relaxed);
    let mut partial = PARTIAL.lock().unwrap_or_else(|e| e.into_inner());
    partial.push_str(&fresh);
    // A line the writer has not finished is kept for the next pump, so a step
    // is never performed with half its argument.
    let complete = match partial.rfind('\n') {
        Some(end) => partial.drain(..=end).collect::<String>(),
        None => return,
    };
    drop(partial);
    for line in complete.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match action(line) {
            Some(action) => perform(&action),
            None => eprintln!("rosemary: appkit: {FILE_ENV}: {line:?} is not an action"),
        }
    }
}

/// Run whatever the script has due, once per pump.
pub fn run() {
    static SCRIPT: std::sync::OnceLock<Vec<Step>> = std::sync::OnceLock::new();
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    let script = SCRIPT.get_or_init(|| match std::env::var(ENV) {
        Ok(value) => parse(&value),
        Err(_) => Vec::new(),
    });
    let window = target().is_some();
    if window {
        run_file();
    }
    let mut next = NEXT.load(Ordering::Relaxed);
    if next >= script.len() {
        return;
    }
    // A tray click needs no window, and the program may have none shown.
    let tray_next = matches!(script[next].action, Action::Tray(_));
    if !window && !tray_next {
        return;
    }
    let elapsed = START.get_or_init(Instant::now).elapsed().as_secs_f64();
    while next < script.len() && script[next].at <= elapsed {
        if target().is_none() && !matches!(script[next].action, Action::Tray(_)) {
            break;
        }
        perform(&script[next].action);
        next += 1;
        NEXT.store(next, Ordering::Relaxed);
    }
}

/// The window the next step drives.
///
/// Asked once per step rather than once per turn of the pump, because a
/// `window` step chooses a different one and the steps that follow it - in the
/// same batch of lines, usually - are exactly the ones that have to go there.
///
/// `ROSEMARY_DEBUG_INPUT_WINDOW` says which: `key` for the key window, for a
/// dialog; `main` for the largest shown one, for an application window the
/// compositor has not given focus to; `top` for the most recently shown, for a
/// menu; a number for the SDL window with that id; the first window made
/// otherwise.
fn target() -> Option<u32> {
    target_window().map(sdl3::video::window_id)
}

/// The SDL window [`target`] names.
fn target_window() -> Option<sdl3::Window> {
    static WHICH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let which = WHICH.get_or_init(|| std::env::var(WINDOW_ENV).unwrap_or_default());
    // A modal dialog is the only window taking input while it is up.
    if let Some(dialog) = crate::dialog::modal_window() {
        return Some(dialog);
    }
    let chosen = CHOSEN.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match chosen.as_deref().unwrap_or(which) {
        // SAFETY: null or a window this crate made.
        "key" => Some(unsafe { crate::window::sdl_window(crate::window::key_window()) })
            .filter(|sdl| !sdl.is_null()),
        // The biggest window on the screen, which is the one a program's own
        // interface is in: a script that drives an application whose window
        // the compositor has not given focus to cannot ask for the key one.
        "main" => main_window(),
        // The innermost open menu of the chrome's, which is no NSWindow.
        "menu" => crate::decor::menu_popup(),
        // The titlebar a full-screen window slides in at the top edge.
        "reveal" => crate::decor::reveal_popup(),
        // The most recently shown window, which is the menu or dialog that has
        // just opened: the one a script has to drive to answer whether a popup
        // responds to the pointer at all.
        "top" => crate::window::all()
            .into_iter()
            .filter(|window| crate::window::is_shown(*window))
            // SAFETY: a window this crate made.
            .map(|window| unsafe { crate::window::sdl_window(window) })
            .rfind(|sdl| !sdl.is_null()),
        // An SDL window id, as `ROSEMARY_TRACE_WINDOW` prints it: the one way
        // to name a window that is neither key, largest nor newest.
        which if which.parse::<u32>().is_ok() => {
            let window = sdl3::video::window_from_id(which.parse().unwrap_or(0));
            (!window.is_null()).then_some(window)
        }
        _ => crate::window::sdl_windows().into_iter().next(),
    }
}

/// The largest shown window this runtime has made.
fn main_window() -> Option<sdl3::Window> {
    crate::window::all()
        .into_iter()
        .filter(|window| crate::window::is_shown(*window))
        // SAFETY: a window this crate made.
        .map(|window| unsafe { crate::window::sdl_window(window) })
        .filter(|sdl| !sdl.is_null())
        .max_by_key(|sdl| {
            sdl3::video::window_size(*sdl).map_or(0, |(w, h)| i64::from(w) * i64::from(h))
        })
}

/// The window a `window` step chose, which overrides [`WINDOW_ENV`].
static CHOSEN: Mutex<Option<String>> = Mutex::new(None);

fn perform(action: &Action) {
    eprintln!("rosemary: appkit: {ENV}: {action:?}");
    // And to `ROSEMARY_EVENT_LOG`, which a guest that replaces its own stderr
    // cannot take with it.
    crate::event::debug_log(&format!("script {action:?}"));
    if let Action::Tray(label) = action {
        if !crate::status::click(label) {
            eprintln!("rosemary: appkit: {ENV}: no tray entry is labelled {label:?}");
        }
        return;
    }
    let Some(window) = target() else {
        return;
    };
    let (x, y) = *POINTER.lock().unwrap_or_else(|e| e.into_inner());
    match action {
        Action::Move(to_x, to_y) => {
            let (dx, dy) = (to_x - x, to_y - y);
            *POINTER.lock().unwrap_or_else(|e| e.into_inner()) = (*to_x, *to_y);
            MOVED.store(true, Ordering::Relaxed);
            crate::event::deliver(Event::MouseMotion {
                window,
                x: *to_x,
                y: *to_y,
                dx,
                dy,
                // A move with a button held is a drag, as it is from a real
                // mouse: SDL reports the held buttons on its motion events.
                buttons: HELD.load(Ordering::Relaxed),
            });
        }
        Action::Click { button, clicks } => {
            for click in 1..=*clicks {
                button_event(window, *button, true, click, x, y);
                button_event(window, *button, false, click, x, y);
            }
        }
        Action::Down(button) => button_event(window, *button, true, 1, x, y),
        Action::Up(button) => button_event(window, *button, false, 1, x, y),
        Action::Scroll(dx, dy) => crate::event::deliver(Event::MouseWheel {
            window,
            x: *dx,
            y: *dy,
        }),
        Action::Key {
            scancode,
            character,
            modifiers,
        } => key(window, *scancode, *character, *modifiers),
        Action::Hold {
            scancode,
            character,
            modifiers,
            down,
        } => crate::event::deliver(Event::Key {
            window,
            scancode: *scancode,
            key: *character,
            modifiers: *modifiers,
            down: *down,
            repeat: false,
        }),
        Action::Tray(_) => {}
        Action::Window(name) => {
            *CHOSEN.lock().unwrap_or_else(|e| e.into_inner()) = Some(name.clone());
        }
        Action::Type(text) => {
            for character in text.chars() {
                match typed(character) {
                    Some((scancode, shifted)) => key(
                        window,
                        scancode,
                        character as u32,
                        match shifted {
                            // `SDL_KMOD_LSHIFT`, so `-modifierFlags` reports the
                            // shift a capital letter was typed with.
                            true => 0x0001,
                            false => 0,
                        },
                    ),
                    None => eprintln!(
                        "rosemary: appkit: {ENV}: no US key types {character:?} - skipped"
                    ),
                }
            }
        }
    }
}

/// SDL's mask of the buttons the script holds down, `1 << (button - 1)` each.
static HELD: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn button_event(window: u32, button: u8, down: bool, clicks: i64, x: f32, y: f32) {
    let bit = 1u32 << u32::from(button.max(1) - 1);
    match down {
        true => HELD.fetch_or(bit, Ordering::Relaxed),
        false => HELD.fetch_and(!bit, Ordering::Relaxed),
    };
    crate::event::deliver(Event::MouseButton {
        window,
        button: button.into(),
        down,
        clicks: clicks as u8,
        x,
        y,
    });
}

fn key(window: u32, scancode: u32, character: u32, modifiers: u16) {
    let event = |down| Event::Key {
        window,
        scancode,
        key: character,
        modifiers,
        down,
        repeat: false,
    };
    // A real Control chord has Control's own press and release around it,
    // and the guest (and an editing chord) follow Control from those.
    let control = |down: bool| {
        use sdl3::keycodes::{keymod, scancode};
        let mut held = match down {
            true => modifiers & !keymod::CTRL,
            false => modifiers,
        };
        for (side, code) in [
            (keymod::LCTRL, scancode::LCTRL),
            (keymod::RCTRL, scancode::RCTRL),
        ] {
            if modifiers & side == 0 {
                continue;
            }
            held = match down {
                true => held | side,
                false => held & !side,
            };
            crate::event::deliver(Event::Key {
                window,
                scancode: code,
                // `SDLK_SCANCODE_MASK | scancode`, SDL's keycode for it.
                key: 0x4000_0000 | code,
                modifiers: held,
                down,
                repeat: false,
            });
        }
    };
    // SDL places a committed text event between the physical press and
    // release, and the pump carries it on the key down. Keep scripted typing
    // on the same path as a real keyboard, which commits no text for a
    // Control or Command chord.
    let chord = modifiers & (0x0040 | 0x0080 | 0x0400 | 0x0800) != 0;
    let text = char::from_u32(character)
        .filter(|c| !c.is_control() && !chord)
        .map(String::from);
    control(true);
    // A key a dialog took never became an event, and the dialog reads text
    // from its own events.
    if let Some(text) = crate::event::deliver_typed_key(event(true), text) {
        crate::event::deliver(Event::Text { window, text });
    }
    crate::event::deliver(event(false));
    control(false);
}

/// The US-layout key that types a character, and whether it needs shift.
///
/// A US layout because that is what [`crate::keymap`] already is: a script that
/// typed through a different one would report a key code the guest's own table
/// would read as another key.
fn typed(character: char) -> Option<(u32, bool)> {
    let unshifted = |scancode: u32| Some((scancode, false));
    let shifted = |scancode: u32| Some((scancode, true));
    match character {
        'a'..='z' => unshifted(4 + (character as u32 - 'a' as u32)),
        'A'..='Z' => shifted(4 + (character as u32 - 'A' as u32)),
        '1'..='9' => unshifted(30 + (character as u32 - '1' as u32)),
        '0' => unshifted(39),
        ' ' => unshifted(44),
        '\n' => unshifted(40),
        '\t' => unshifted(43),
        '-' => unshifted(45),
        '=' => unshifted(46),
        '[' => unshifted(47),
        ']' => unshifted(48),
        '\\' => unshifted(49),
        ';' => unshifted(51),
        '\'' => unshifted(52),
        '`' => unshifted(53),
        ',' => unshifted(54),
        '.' => unshifted(55),
        '/' => unshifted(56),
        '!' => shifted(30),
        '@' => shifted(31),
        '#' => shifted(32),
        '$' => shifted(33),
        '%' => shifted(34),
        '^' => shifted(35),
        '&' => shifted(36),
        '*' => shifted(37),
        '(' => shifted(38),
        ')' => shifted(39),
        '_' => shifted(45),
        '+' => shifted(46),
        '{' => shifted(47),
        '}' => shifted(48),
        '|' => shifted(49),
        ':' => shifted(51),
        '"' => shifted(52),
        '~' => shifted(53),
        '<' => shifted(54),
        '>' => shifted(55),
        '?' => shifted(56),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_key_is_pressed_and_released_by_separate_steps() {
        let steps = parse("1: keydown w; 2: keyup w");
        let down = |down| Action::Hold {
            scancode: 26,
            character: 'w' as u32,
            modifiers: 0,
            down,
        };
        assert_eq!(steps[0].action, down(true));
        assert_eq!(steps[1].action, down(false));
    }

    #[test]
    fn a_script_is_read_in_time_order_whatever_order_it_is_written_in() {
        let steps = parse("9: click; 8: move 10,20; 10: type hi");
        let times: Vec<f64> = steps.iter().map(|s| s.at).collect();
        assert_eq!(times, [8.0, 9.0, 10.0]);
        assert_eq!(steps[0].action, Action::Move(10.0, 20.0));
        assert_eq!(steps[2].action, Action::Type("hi".to_owned()));
    }

    /// A step that parses to nothing is dropped rather than shifting the ones
    /// after it, so a typo costs one action and not the whole script.
    #[test]
    fn an_unknown_action_is_dropped_and_the_rest_still_run() {
        let steps = parse("1: teleport 5,5; 2: click");
        assert_eq!(steps.len(), 1);
        assert_eq!(
            steps[0].action,
            Action::Click {
                button: 1,
                clicks: 1
            }
        );
    }

    /// The shift is part of the key, not of the character: a guest reads
    /// `-modifierFlags` and `-characters` off the same event and a capital typed
    /// without the flag is a key nothing was holding shift for.
    #[test]
    fn a_capital_is_the_same_key_with_shift() {
        assert_eq!(typed('a'), Some((4, false)));
        assert_eq!(typed('A'), Some((4, true)));
        assert_eq!(typed('@'), Some((31, true)), "shift-2");
        assert_eq!(typed('\u{2603}'), None, "no US key types a snowman");
    }
}
