use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Instant;

use super::dock::Field;
use super::draw::{Color, Ui, color, duration};

/// The template recorded when nothing else is asked for.
const DEFAULT_TEMPLATE: &str = "Time Profiler";

/// A recording in flight.
struct Recording {
    child: Child,
    started: Instant,
    output: String,
}

/// What the overlay is doing about tracing, and the last thing it said.
#[derive(Default)]
struct State {
    recording: Option<Recording>,
    /// The last outcome, and when - shown on the panel and as a badge for a few
    /// seconds, because a keystroke that silently did nothing is worse than no
    /// keystroke at all.
    said: Option<(Instant, String, Color)>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Where recordings are written, once anyone has said.
///
/// Its own lock rather than a field of [`State`]: the panel reads it while it
/// is drawing, and the editor writes it from the event thread.
static DIRECTORY: Mutex<Option<String>> = Mutex::new(None);

/// The directory recordings go to: what the panel set, else the environment,
/// else the working directory.
pub fn directory() -> String {
    if let Some(set) = DIRECTORY.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return set.clone();
    }
    std::env::var("ROSEMARY_GUI_TRACE_DIR").unwrap_or_else(|_| ".".into())
}

/// Point recordings somewhere else. An empty path means the working directory.
pub fn set_directory(path: &str) {
    let path = path.trim();
    let mut guard = DIRECTORY.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(match path.is_empty() {
        true => ".".to_string(),
        false => path.trim_end_matches('/').to_string(),
    });
}

/// The template being recorded.
fn template() -> String {
    std::env::var("ROSEMARY_GUI_TEMPLATE").unwrap_or_else(|_| DEFAULT_TEMPLATE.to_string())
}

/// How long a message stays on screen after the keystroke that produced it.
const TOAST: std::time::Duration = std::time::Duration::from_secs(6);

/// Start a recording, or stop the one in flight. Bound to shift+F6.
pub fn toggle() {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.get_or_insert_with(State::default);
    match state.recording.is_some() {
        true => stop(state),
        false => start(state),
    }
}

/// Whether a recording is in flight, and for how long.
pub fn recording_for() -> Option<std::time::Duration> {
    let guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.as_ref()?;
    state.recording.as_ref().map(|r| r.started.elapsed())
}

/// The message to show as a badge, if there is a recent one.
pub fn toast() -> Option<(String, Color)> {
    let guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let (at, message, colour) = guard.as_ref()?.said.as_ref()?;
    (at.elapsed() < TOAST).then(|| (message.clone(), *colour))
}

fn say(state: &mut State, message: impl Into<String>, colour: Color) {
    let message = message.into();
    // On stderr as well as on screen.
    eprintln!("rosemary: gui  {message}");
    state.said = Some((Instant::now(), message, colour));
}

fn start(state: &mut State) {
    let Some(xctrace) = xctrace() else {
        say(
            state,
            "no working xctrace: this needs the full Xcode, not the Command Line Tools",
            color::ALERT,
        );
        return;
    };
    let template = template();
    let output = output_path();

    // `--attach` rather than `--launch`.
    let child = Command::new(xctrace)
        .arg("record")
        .arg("--template")
        .arg(&template)
        .arg("--output")
        .arg(&output)
        .arg("--attach")
        .arg(std::process::id().to_string())
        .stdin(Stdio::null())
        .spawn();

    match child {
        Ok(child) => {
            state.recording = Some(Recording {
                child,
                started: Instant::now(),
                output: output.clone(),
            });
            say(
                state,
                format!("recording '{template}' to {output} - shift+F6 stops"),
                color::ALERT,
            );
        }
        Err(e) => say(state, format!("could not start xctrace: {e}"), color::ALERT),
    }
}

fn stop(state: &mut State) {
    let Some(mut recording) = state.recording.take() else {
        return;
    };
    let elapsed = recording.started.elapsed();

    // `SIGINT`, not `kill`. `xctrace` finalizes the trace on an interrupt and
    // discards it on anything harsher, and a recording that cannot be opened is
    // indistinguishable from one that never happened.
    //
    // SAFETY: `kill` with a pid this process spawned and a signal number that
    // exists. A child that has already exited gives `ESRCH`, which is ignored.
    unsafe { libc::kill(recording.child.id() as i32, libc::SIGINT) };
    let status = recording.child.wait();

    // The map, from the run that was just recorded, beside the trace it belongs
    // to.
    let map = format!("{}.map", recording.output.trim_end_matches(".trace"));
    let map_note = match crate::profile::write_map(&map) {
        Ok(()) => format!(", map at {map}"),
        Err(e) => format!(", but the map could not be written: {e}"),
    };

    match status {
        Ok(status) if status.success() => say(
            state,
            format!(
                "recorded {} to {}{map_note}",
                duration(elapsed),
                recording.output,
            ),
            color::GUEST,
        ),
        // A non-zero exit is where the two well-known refusals land.
        Ok(status) => say(
            state,
            format!("xctrace exited {status}: see its message above"),
            color::ALERT,
        ),
        Err(e) => say(
            state,
            format!("xctrace could not be waited for: {e}"),
            color::ALERT,
        ),
    }
}

/// Where this recording goes.
///
/// The working directory, named for the guest and the process, so that two runs and two
/// recordings inside one run never collide.
fn output_path() -> String {
    let program = super::program_name().unwrap_or("guest");
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let directory = directory();
    format!("{directory}/{program}.{}.{stamp}.trace", std::process::id())
}

/// What the search for a usable `xctrace` found, once it has run.
static FOUND: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();

/// The answer, if it is already known. Never blocks.
///
/// The search runs `xctrace version`, and spawning a process is not something a
/// render thread can be made to wait for: the panel asks this every frame and
/// says nothing until [`probe`] has an answer.
fn found() -> Option<Option<&'static str>> {
    FOUND.get().copied()
}

/// Start the search, off the render thread, at most once.
fn probe() {
    static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if ASKED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("rosemary-gui-xctrace".into())
        .spawn(|| {
            xctrace();
        });
    if spawned.is_err() {
        super::say_once("could not look for xctrace: the probe thread would not start");
    }
}

/// A working `xctrace`, found by running one.
fn xctrace() -> Option<&'static str> {
    *FOUND.get_or_init(|| {
        [
            "xctrace",
            "/Applications/Xcode.app/Contents/Developer/usr/bin/xctrace",
        ]
        .into_iter()
        .find(|candidate| {
            Command::new(candidate)
                .arg("version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
    })
}

/// The xctrace window: where recordings go, and the button that takes one.
pub fn window(ui: &mut Ui) {
    ui.heading("XCTRACE");

    // The buffer while it is being typed into, so a half-typed path is not
    // written to the setting behind it on every keystroke.
    let editing = super::dock::editing(Field::TraceDir);
    let shown = editing.clone().unwrap_or_else(directory);
    if ui.field("trace directory", &shown, editing.is_some()) {
        match editing {
            // A second click on the box being edited keeps the caret there.
            Some(_) => {}
            None => super::dock::begin_edit(Field::TraceDir, &shown),
        }
    }
    match editing.is_some() {
        true => ui.line("return keeps it, escape puts it back", color::MUTED),
        false => ui.line("click the box to type a path", color::MUTED),
    }
    ui.row("template", &template(), color::VALUE);

    let (recording, output) = {
        let guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref().and_then(|s| s.recording.as_ref()) {
            Some(r) => (Some(r.started.elapsed()), r.output.clone()),
            None => (None, String::new()),
        }
    };

    ui.gap(0.4);
    let pressed = match recording {
        Some(_) => ui.button("stop recording", true),
        None => ui.button("start recording", false),
    };
    ui.newline();
    ui.gap(0.3);
    if pressed {
        toggle();
    }

    match recording {
        Some(elapsed) => {
            ui.row("recording", &duration(elapsed), color::ALERT);
            ui.row("into", &output, color::VALUE);
            ui.line("the map is written beside it when this stops", color::MUTED);
        }
        None => {
            ui.row(
                "attaches to",
                &format!("pid {}", std::process::id()),
                color::VALUE,
            );
            ui.line(
                "records cycles, branch misses and cache misses",
                color::MUTED,
            );
            ui.line(
                "needs the full Xcode, and the driver signed with get-task-allow",
                color::MUTED,
            );
            probe();
            match found() {
                Some(None) => ui.line(
                    "no working xctrace: this needs Xcode, not the Command Line Tools",
                    color::ALERT,
                ),
                Some(Some(path)) => ui.row("xctrace", path, color::MUTED),
                None => ui.line("looking for xctrace", color::MUTED),
            }
        }
    }

    // The last outcome, kept on screen a little after the keystroke that
    // produced it: a button that silently did nothing is worse than no button.
    let said = {
        let guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .and_then(|s| s.said.as_ref())
            .filter(|(at, ..)| at.elapsed() < TOAST)
            .map(|(_, message, colour)| (message.clone(), *colour))
    };
    if let Some((message, colour)) = said {
        ui.gap(0.3);
        ui.line(&message, colour);
    }
}
