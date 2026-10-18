use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::input::{Axis, AxisSource, ButtonState, KeyState};
use smithay::backend::renderer::utils::{RendererSurfaceStateUserData, on_commit_buffer_handler};
use smithay::desktop::utils::under_from_surface_tree;
use smithay::desktop::{
    PopupKind, PopupManager, WindowSurfaceType, find_popup_root_surface, get_popup_toplevel_coords,
};
use smithay::input::keyboard::{FilterResult, KeyboardHandle, Keycode, XkbConfig};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent, PointerHandle,
};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::channel::{self, Channel, Sender};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode as Trigger, PostAction};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{
    ClientData, ClientId, DisconnectReason, GlobalId,
};
use smithay::reexports::wayland_server::protocol::wl_shm::Format;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_seat};
use smithay::reexports::wayland_server::{
    Client, Display, DisplayHandle, ListeningSocket, Resource,
};
use smithay::utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial, Transform};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    BufferAssignment, CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
    TraversalAction, add_blocker, add_pre_commit_hook, get_parent, send_surface_state, with_states,
    with_surface_tree_downward,
};
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use smithay::wayland::dmabuf::{
    DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier, get_dmabuf,
};
use smithay::wayland::fractional_scale::{
    FractionalScaleHandler, FractionalScaleManagerState, with_fractional_scale,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
    request_data_device_client_selection, set_data_device_focus, set_data_device_selection,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler,
    XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState, with_buffer_contents};
use smithay::wayland::viewporter::ViewporterState;

use super::composite::{self, Canvas, Rect};
use super::gpu::{Gpu, Pool};
use super::{Command, Composition, Event, Frame, Monitor, Shared, SurfaceId, lock, monitor};

/// How often a window on no known display has its frame callbacks released.
const DEFAULT_REFRESH: u32 = 60_000;

/// How long an output no window is on stays, so that a window moving back
/// and forth between displays does not make GTK remake its monitors.
const OUTPUT_GRACE: Duration = Duration::from_secs(5);

/// How long a toplevel may go without a view claiming it before it is closed.
const UNCLAIMED_GRACE: Duration = Duration::from_secs(3);

/// The text types asked of a selection GTK sets, most preferred first; the
/// first one it offers is read.
const TEXT_TYPES: &[&str] = &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
const HTML_TYPE: &str = "text/html";

#[derive(Clone, Copy, PartialEq)]
struct Configure {
    width: i32,
    height: i32,
    scale: f64,
    suspended: bool,
    /// Whether the window's pictures may be dma-bufs.
    dmabuf: bool,
}

struct Window {
    toplevel: ToplevelSurface,
    configure: Option<Configure>,
    /// The display the window is on, as its view last said.
    monitor: Option<Monitor>,
    /// Which of `State::outputs` it has entered; `None` is the default one.
    output: Option<u64>,
    /// How often its frame callbacks are released: its display's refresh.
    interval: Duration,
    /// When they were last released.
    released: Instant,
    /// Whether a release is already scheduled.
    timer: bool,
    /// The buffers it is composed into on the GPU.
    pool: Pool,
    dirty: bool,
    created: Instant,
    closed: bool,
}

/// An output standing for one host display at one mode and scale. A display
/// whose mode changes gets a new one, rather than the old one changing: a
/// GTK monitor's refresh rate is read once, when WebKit starts pacing a page
/// for it, and only moving to another monitor makes it read it again.
struct Shown {
    id: u64,
    monitor: Monitor,
    scale: f64,
    output: Output,
    global: GlobalId,
}

pub struct State {
    display: DisplayHandle,
    handle: LoopHandle<'static, State>,
    shared: Arc<Shared>,
    /// Where windows are composed, when not on the CPU.
    gpu: Option<Gpu>,
    dmabuf: DmabufState,
    compositor: CompositorState,
    xdg: XdgShellState,
    shm: ShmState,
    seat_state: SeatState<State>,
    data_device: DataDeviceState,
    _outputs: OutputManagerState,
    _viewporter: ViewporterState,
    _fractional: FractionalScaleManagerState,
    _cursor_shape: CursorShapeManagerState,
    seat: Seat<State>,
    /// Absent when xkbcommon cannot find its keymap data; the seat then has
    /// no keyboard capability and pages get pointer input only.
    keyboard: Option<KeyboardHandle<State>>,
    pointer: PointerHandle<State>,
    /// Where a window is until its view says which display it is on.
    output: Output,
    outputs: Vec<Shown>,
    next_output: u64,
    /// The host's connectors, read again whenever a display is seen that no
    /// output stands for yet.
    connectors: Vec<monitor::Connector>,
    popups: PopupManager,
    windows: HashMap<SurfaceId, Window>,
    /// Configures that arrived before their toplevel did.
    early: HashMap<SurfaceId, (Configure, Option<Monitor>)>,
    /// Popups holding a grab, innermost last.
    grabs: Vec<PopupSurface>,
    /// Buttons whose press dismissed a grab, so their release is not sent.
    swallowed: Vec<u32>,
    keyboard_window: Option<SurfaceId>,
    pointer_window: Option<SurfaceId>,
    start: Instant,
}

#[derive(Default)]
struct ClientState {
    compositor: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client: ClientId) {}
    fn disconnected(&self, _client: ClientId, _reason: DisconnectReason) {}
}

fn id(surface: &WlSurface) -> SurfaceId {
    surface.id().protocol_id()
}

type Started = Result<(Sender<Command>, bool), String>;

pub(super) fn run(
    socket: PathBuf,
    shared: Arc<Shared>,
    composition: Composition,
    reply: mpsc::Sender<Started>,
) {
    let started = start(socket, shared, composition);
    match started {
        Err(why) => {
            let _ = reply.send(Err(why));
        }
        Ok((mut event_loop, mut state, sender)) => {
            let _ = reply.send(Ok((sender, state.gpu.is_some())));
            let _ = event_loop.run(None, &mut state, State::after_dispatch);
        }
    }
}

fn start(
    socket: PathBuf,
    shared: Arc<Shared>,
    composition: Composition,
) -> Result<(EventLoop<'static, State>, State, Sender<Command>), String> {
    let event_loop: EventLoop<'static, State> =
        EventLoop::try_new().map_err(|e| format!("no event loop: {e}"))?;
    let display: Display<State> = Display::new().map_err(|e| format!("no display: {e}"))?;
    let listener = ListeningSocket::bind_absolute(socket.clone())
        .map_err(|e| format!("cannot listen at {}: {e}", socket.display()))?;
    let (sender, commands): (Sender<Command>, Channel<Command>) = channel::channel();
    let handle = event_loop.handle();
    // Made on this thread, which its GL context is current on for good.
    let gpu = match composition {
        Composition::Cpu => None,
        Composition::Gpu(importer) => match Gpu::new(importer.as_ref()) {
            Ok(gpu) => Some(gpu),
            Err(why) => {
                eprintln!(
                    "rosemary: webkit: compositor: composing on the CPU, as the GPU cannot be used: {why}"
                );
                None
            }
        },
    };
    let state = State::new(display.handle(), handle.clone(), shared, gpu);

    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Trigger::Level),
            |_, listener, state| {
                let Ok(Some(stream)) = listener.accept() else {
                    return Ok(PostAction::Continue);
                };
                if let Err(why) = state
                    .display
                    .insert_client(stream, Arc::new(ClientState::default()))
                {
                    eprintln!("rosemary: webkit: compositor: GTK could not connect: {why}");
                    return Ok(PostAction::Continue);
                }
                // One client, and nobody else should find the socket.
                Ok(PostAction::Remove)
            },
        )
        .map_err(|e| format!("cannot watch the socket: {e}"))?;
    handle
        .insert_source(
            Generic::new(display, Interest::READ, Trigger::Level),
            |_, display, state| {
                // SAFETY: the display is not dropped while its source is live.
                if let Err(why) = unsafe { display.get_mut() }.dispatch_clients(state) {
                    eprintln!("rosemary: webkit: compositor: {why}");
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|e| format!("cannot watch the display: {e}"))?;
    handle
        .insert_source(commands, |event, _, state| {
            if let channel::Event::Msg(command) = event {
                state.command(command);
            }
        })
        .map_err(|e| format!("cannot watch for commands: {e}"))?;
    handle
        .insert_source(Timer::from_duration(UNCLAIMED_GRACE), |_, _, state| {
            state.close_unclaimed();
            TimeoutAction::ToDuration(UNCLAIMED_GRACE)
        })
        .map_err(|e| format!("cannot start a timer: {e}"))?;
    Ok((event_loop, state, sender))
}

impl State {
    fn new(
        display: DisplayHandle,
        handle: LoopHandle<'static, State>,
        shared: Arc<Shared>,
        mut gpu: Option<Gpu>,
    ) -> State {
        let compositor = CompositorState::new::<State>(&display);
        let mut dmabuf = DmabufState::new();
        if let Some(found) = gpu.as_ref() {
            match found.feedback() {
                Ok(feedback) => {
                    dmabuf.create_global_with_default_feedback::<State>(&display, &feedback);
                }
                Err(why) => {
                    eprintln!("rosemary: webkit: compositor: composing on the CPU: {why}");
                    gpu = None;
                }
            }
        }
        let xdg = XdgShellState::new::<State>(&display);
        let shm = ShmState::new::<State>(&display, Vec::new());
        let mut seat_state = SeatState::new();
        let data_device = DataDeviceState::new::<State>(&display);
        let outputs = OutputManagerState::new_with_xdg_output::<State>(&display);
        let viewporter = ViewporterState::new::<State>(&display);
        let fractional = FractionalScaleManagerState::new::<State>(&display);
        let cursor_shape = CursorShapeManagerState::new::<State>(&display);
        let mut seat = seat_state.new_wl_seat(&display, "seat0");
        // An empty configuration is xkbcommon's defaults, which honour
        // `XKB_DEFAULT_LAYOUT` and the rest.
        let keyboard = seat
            .add_keyboard(XkbConfig::default(), 600, 25)
            .or_else(|_| {
                seat.add_keyboard(
                    XkbConfig {
                        layout: "us",
                        ..XkbConfig::default()
                    },
                    600,
                    25,
                )
            })
            .map_err(|err| {
                eprintln!(
                    "rosemary: webkit: compositor: no keyboard, web views cannot be typed into: \
                     {err} (point XKB_CONFIG_ROOT at xkeyboard-config's xkb directory)"
                );
            })
            .ok();
        let pointer = seat.add_pointer();
        let output = Output::new(
            "rosemary".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Rosemary".to_string(),
                model: "WebKit".to_string(),
            },
        );
        let mode = Mode {
            size: (1920, 1080).into(),
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Integer(1)),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        let _ = output.create_global::<State>(&display);
        State {
            display,
            handle,
            shared,
            gpu,
            dmabuf,
            compositor,
            xdg,
            shm,
            seat_state,
            data_device,
            _outputs: outputs,
            _viewporter: viewporter,
            _fractional: fractional,
            _cursor_shape: cursor_shape,
            seat,
            keyboard,
            pointer,
            output,
            outputs: Vec::new(),
            next_output: 0,
            connectors: Vec::new(),
            popups: PopupManager::default(),
            windows: HashMap::new(),
            early: HashMap::new(),
            grabs: Vec::new(),
            swallowed: Vec::new(),
            keyboard_window: None,
            pointer_window: None,
            start: Instant::now(),
        }
    }

    fn time(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// Publish what changed and get it to GTK.
    fn after_dispatch(&mut self) {
        self.popups.cleanup();
        let mut published = false;
        let dirty: Vec<SurfaceId> = self
            .windows
            .iter()
            .filter(|(_, w)| w.dirty)
            .map(|(id, _)| *id)
            .collect();
        for window in dirty {
            let Some(entry) = self.windows.get_mut(&window) else {
                continue;
            };
            let composed = match self.gpu.as_mut() {
                Some(gpu) => {
                    let (scale, dmabuf) = entry
                        .configure
                        .map_or((1.0, false), |c| (c.scale, c.dmabuf));
                    gpu.compose(&entry.toplevel, scale, &mut entry.pool, !dmabuf)
                }
                None => compose(&entry.toplevel),
            };
            // Nothing drawn - no buffer yet, or none free - is tried again
            // after the next thing that happens.
            entry.dirty = composed.is_none();
            if let Some(frame) = composed {
                lock(&self.shared.frames).insert(window, frame);
                published = true;
            }
        }
        if published || !lock(&self.shared.events).is_empty() {
            (self.shared.wake)();
        }
        if let Err(why) = self.display.flush_clients() {
            eprintln!("rosemary: webkit: compositor: {why}");
        }
    }

    /// The window `surface` is part of: itself, one of its subsurfaces, or a
    /// popup of either.
    fn window_of(&self, surface: &WlSurface) -> Option<SurfaceId> {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        if let Some(popup) = self.popups.find_popup(&root) {
            root = find_popup_root_surface(&popup).ok()?;
        }
        let window = id(&root);
        self.windows.contains_key(&window).then_some(window)
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Configure {
                surface,
                width,
                height,
                scale,
                suspended,
                monitor,
                dmabuf,
            } => {
                let configure = Configure {
                    width: width.max(1),
                    height: height.max(1),
                    scale: scale.max(1.0),
                    suspended,
                    dmabuf,
                };
                match self.windows.get_mut(&surface) {
                    Some(window) => {
                        let moved = window.monitor != monitor;
                        window.monitor = monitor;
                        let changed = window.configure != Some(configure);
                        if changed {
                            // Pictures of the other kind are wanted now, and
                            // GTK may not draw again for a while.
                            if window.configure.is_some_and(|c| c.dmabuf != dmabuf) {
                                window.dirty = true;
                            }
                            window.configure = Some(configure);
                            self.configure(surface);
                        }
                        if moved || changed {
                            self.place(surface);
                        }
                    }
                    None => {
                        self.early.insert(surface, (configure, monitor));
                    }
                }
                self.arm_frames(surface);
            }
            Command::Taken { surface } => self.arm_frames(surface),
            Command::Forget { surface } => {
                self.early.remove(&surface);
                if self.keyboard_window == Some(surface) {
                    self.keyboard_window = None;
                }
                if self.pointer_window == Some(surface) {
                    self.pointer_window = None;
                }
            }
            Command::Pointer { surface, x, y } => self.pointer_motion(surface, (x, y).into()),
            Command::Button { button, pressed } => self.button(button, pressed),
            Command::Axis { dx, dy, steps } => {
                let mut frame = AxisFrame::new(self.time());
                frame = match steps {
                    Some((sx, sy)) => {
                        let mut frame = frame.source(AxisSource::Wheel);
                        if sx != 0 {
                            frame = frame.v120(Axis::Horizontal, sx);
                        }
                        if sy != 0 {
                            frame = frame.v120(Axis::Vertical, sy);
                        }
                        frame
                    }
                    None => frame.source(AxisSource::Continuous),
                };
                if dx != 0.0 {
                    frame = frame.value(Axis::Horizontal, dx);
                }
                if dy != 0.0 {
                    frame = frame.value(Axis::Vertical, dy);
                }
                let pointer = self.pointer.clone();
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            Command::Key { key, pressed } => {
                // A release of a key the page never saw pressed: a shortcut
                // the menus took, whose key-up still reaches the view.
                let Some(keyboard) = self.keyboard.clone() else {
                    return;
                };
                if !pressed && !keyboard.pressed_keys().contains(&Keycode::new(key + 8)) {
                    return;
                }
                let state = match pressed {
                    true => KeyState::Pressed,
                    false => KeyState::Released,
                };
                let time = self.time();
                // Evdev codes are X keycodes less 8.
                keyboard.input::<(), _>(
                    self,
                    Keycode::new(key + 8),
                    state,
                    SERIAL_COUNTER.next_serial(),
                    time,
                    |_, _, _| FilterResult::Forward,
                );
            }
            Command::Focus { surface } => {
                let old = std::mem::replace(&mut self.keyboard_window, surface);
                self.focus_keyboard();
                for window in [old, surface].into_iter().flatten() {
                    self.configure(window);
                }
            }
            Command::Offer { entries } => {
                if entries.is_empty() {
                    return;
                }
                let types = entries.iter().map(|(ty, _)| ty.clone()).collect();
                set_data_device_selection(&self.display, &self.seat, types, Arc::new(entries));
            }
        }
    }

    /// Give the keyboard to the focused window, or to the innermost popup
    /// holding a grab on it.
    fn focus_keyboard(&mut self) {
        let target = self.keyboard_window.and_then(|window| {
            let popup = self
                .grabs
                .last()
                .filter(|popup| self.window_of(popup.wl_surface()) == Some(window))
                .map(|popup| popup.wl_surface().clone());
            popup.or_else(|| {
                self.windows
                    .get(&window)
                    .map(|w| w.toplevel.wl_surface().clone())
            })
        });
        let Some(keyboard) = self.keyboard.clone() else {
            return;
        };
        if keyboard.current_focus() == target {
            return;
        }
        keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
    }

    /// Send `window` its size, scale and states, once it has committed.
    fn configure(&mut self, window: SurfaceId) {
        let focused = self.keyboard_window == Some(window);
        let Some(entry) = self.windows.get(&window) else {
            return;
        };
        let configure = entry.configure;
        let toplevel = entry.toplevel.clone();
        toplevel.with_pending_state(|state| {
            // Maximized and tiled on every edge: GTK then takes the size it is
            // given exactly and draws no shadow around it.
            for fixed in [
                xdg_toplevel::State::Maximized,
                xdg_toplevel::State::TiledLeft,
                xdg_toplevel::State::TiledRight,
                xdg_toplevel::State::TiledTop,
                xdg_toplevel::State::TiledBottom,
            ] {
                state.states.set(fixed);
            }
            match focused {
                true => state.states.set(xdg_toplevel::State::Activated),
                false => state.states.unset(xdg_toplevel::State::Activated),
            };
            match configure.is_some_and(|c| c.suspended) {
                true => state.states.set(xdg_toplevel::State::Suspended),
                false => state.states.unset(xdg_toplevel::State::Suspended),
            };
            if let Some(configure) = configure {
                state.size = Some((configure.width, configure.height).into());
            }
        });
        if let Some(configure) = configure {
            set_scale(toplevel.wl_surface(), configure.scale);
        }
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn scale_of(&self, window: SurfaceId) -> Option<f64> {
        self.windows
            .get(&window)
            .and_then(|w| w.configure)
            .map(|c| c.scale)
    }

    /// The surface under `point` in `window`, with its origin.
    fn under(
        &self,
        window: SurfaceId,
        point: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let root = self.windows.get(&window)?.toplevel.wl_surface().clone();
        let popups: Vec<(PopupKind, Point<i32, Logical>)> =
            PopupManager::popups_for_surface(&root).collect();
        for (popup, offset) in popups.iter().rev() {
            let origin = *offset - popup.geometry().loc;
            if let Some((surface, at)) =
                under_from_surface_tree(popup.wl_surface(), point, origin, WindowSurfaceType::ALL)
            {
                return Some((surface, at.to_f64()));
            }
        }
        let origin = Point::default() - geometry(&root).loc;
        under_from_surface_tree(&root, point, origin, WindowSurfaceType::ALL)
            .map(|(surface, at)| (surface, at.to_f64()))
    }

    fn pointer_motion(&mut self, window: SurfaceId, point: Point<f64, Logical>) {
        self.pointer_window = Some(window);
        let focus = self.under(window, point);
        let pointer = self.pointer.clone();
        let event = MotionEvent {
            location: point,
            serial: SERIAL_COUNTER.next_serial(),
            time: self.time(),
        };
        pointer.motion(self, focus, &event);
        pointer.frame(self);
    }

    fn button(&mut self, button: u32, pressed: bool) {
        if !pressed && let Some(at) = self.swallowed.iter().position(|b| *b == button) {
            self.swallowed.remove(at);
            return;
        }
        if pressed && !self.grabs.is_empty() {
            let focus = self.pointer.current_focus();
            let inside = focus.is_some_and(|surface| {
                let mut root = surface;
                while let Some(parent) = get_parent(&root) {
                    root = parent;
                }
                self.grabs.iter().any(|popup| *popup.wl_surface() == root)
            });
            if !inside {
                // A press outside the popup dismisses it and goes no further,
                // as it does on every desktop.
                self.dismiss_grabs();
                self.swallowed.push(button);
                return;
            }
        }
        let pointer = self.pointer.clone();
        let event = ButtonEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: self.time(),
            button,
            state: match pressed {
                true => ButtonState::Pressed,
                false => ButtonState::Released,
            },
        };
        pointer.button(self, &event);
        pointer.frame(self);
    }

    fn dismiss_grabs(&mut self) {
        for popup in std::mem::take(&mut self.grabs).into_iter().rev() {
            popup.send_popup_done();
        }
        self.focus_keyboard();
    }

    /// Close the toplevels no view has claimed: a dialog GTK opened that no
    /// window here would ever show.
    fn close_unclaimed(&mut self) {
        for window in self.windows.values_mut() {
            if window.configure.is_none()
                && !window.closed
                && window.created.elapsed() >= UNCLAIMED_GRACE
            {
                let title = with_states(window.toplevel.wl_surface(), |states| {
                    states
                        .data_map
                        .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                        .and_then(|data| data.lock().ok()?.title.clone())
                })
                .unwrap_or_default();
                eprintln!(
                    "rosemary: webkit: closing GTK window {title:?}, which no web view shows"
                );
                window.closed = true;
                window.toplevel.send_close();
            }
        }
    }

    /// Release `window`'s frame callbacks one refresh after they were last
    /// released, unless that is already scheduled.
    fn arm_frames(&mut self, window: SurfaceId) {
        let Some(entry) = self.windows.get_mut(&window) else {
            return;
        };
        if entry.timer {
            return;
        }
        entry.timer = true;
        // After a pause the deadline has passed, and the release is at once.
        let deadline = (entry.released + entry.interval).max(Instant::now());
        let armed =
            self.handle
                .insert_source(Timer::from_deadline(deadline), move |_, _, state| {
                    if let Some(entry) = state.windows.get_mut(&window) {
                        entry.timer = false;
                    }
                    state.release_frames(window);
                    TimeoutAction::Drop
                });
        if armed.is_err()
            && let Some(entry) = self.windows.get_mut(&window)
        {
            entry.timer = false;
        }
    }

    /// Tell `window` it can draw again, if it is shown and its last picture
    /// has been taken; taking it sends `Command::Taken`, which comes back
    /// here.
    fn release_frames(&mut self, window: SurfaceId) {
        let time = self.time();
        let Some(entry) = self.windows.get_mut(&window) else {
            return;
        };
        if entry.configure.is_some_and(|c| c.suspended)
            || lock(&self.shared.frames).contains_key(&window)
        {
            return;
        }
        let root = entry.toplevel.wl_surface();
        let popups: Vec<WlSurface> = PopupManager::popups_for_surface(root)
            .map(|(popup, _)| popup.wl_surface().clone())
            .collect();
        if !has_callbacks(root) && !popups.iter().any(has_callbacks) {
            return;
        }
        entry.released = Instant::now();
        send_frames(root, time);
        for popup in &popups {
            send_frames(popup, time);
        }
        let _ = self.display.flush_clients();
    }

    /// Put `window` on the output for the display it is on, making one if
    /// none stands for it yet, and let go of outputs no window is on.
    fn place(&mut self, window: SurfaceId) {
        let Some(entry) = self.windows.get(&window) else {
            return;
        };
        let Some(monitor) = entry.monitor.clone() else {
            return;
        };
        let scale = entry.configure.map_or(1.0, |c| c.scale);
        let found = self
            .outputs
            .iter()
            .find(|shown| shown.monitor == monitor && shown.scale == scale)
            .map(|shown| shown.id);
        let id = match found {
            Some(id) => id,
            None => self.add_output(monitor.clone(), scale),
        };
        let Some(entry) = self.windows.get_mut(&window) else {
            return;
        };
        entry.interval = interval(monitor.refresh);
        if entry.output == Some(id) {
            return;
        }
        let old = entry.output.replace(id);
        let root = entry.toplevel.wl_surface().clone();
        let mut surfaces = vec![root.clone()];
        surfaces.extend(
            PopupManager::popups_for_surface(&root).map(|(popup, _)| popup.wl_surface().clone()),
        );
        let output_of = |id: Option<u64>| match id {
            None => Some(self.output.clone()),
            Some(id) => self
                .outputs
                .iter()
                .find(|shown| shown.id == id)
                .map(|shown| shown.output.clone()),
        };
        // Entered before the old one is left, so that GTK never sees the
        // window on no monitor at all.
        if let Some(new) = output_of(Some(id)) {
            for surface in &surfaces {
                new.enter(surface);
            }
        }
        if let Some(old) = output_of(old) {
            for surface in &surfaces {
                old.leave(surface);
            }
        }
        if crate::engine::tracing() {
            eprintln!(
                "rosemary: webkit: compositor: window {window} is on {:?} at {} mHz",
                monitor.name, monitor.refresh
            );
        }
        self.retire_outputs();
    }

    fn add_output(&mut self, monitor: Monitor, scale: f64) -> u64 {
        let seen = |connectors: &[monitor::Connector]| {
            monitor
                .name
                .as_ref()
                .is_some_and(|name| connectors.iter().any(|c| c.name == *name))
        };
        if !seen(&self.connectors) {
            self.connectors = monitor::connectors();
        }
        let (width_mm, height_mm) =
            monitor::physical_size(&self.connectors, &monitor).unwrap_or((0, 0));
        let output = Output::new(
            monitor
                .name
                .clone()
                .unwrap_or_else(|| "rosemary".to_string()),
            PhysicalProperties {
                size: (width_mm as i32, height_mm as i32).into(),
                subpixel: Subpixel::Unknown,
                make: "Rosemary".to_string(),
                model: "WebKit".to_string(),
            },
        );
        let mode = Mode {
            size: (monitor.width.max(1), monitor.height.max(1)).into(),
            refresh: monitor.refresh as i32,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Fractional(scale)),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        let global = output.create_global::<State>(&self.display);
        let id = self.next_output;
        self.next_output += 1;
        if crate::engine::tracing() {
            eprintln!(
                "rosemary: webkit: compositor: output {id} for {:?}: {}x{} at {} mHz, {width_mm}x{height_mm} mm",
                monitor.name, monitor.width, monitor.height, monitor.refresh
            );
        }
        self.outputs.push(Shown {
            id,
            monitor,
            scale,
            output,
            global,
        });
        id
    }

    /// Withdraw the outputs no window is on, and destroy them a while later,
    /// once GTK has had time to stop using them.
    fn retire_outputs(&mut self) {
        let unused: Vec<u64> = self
            .outputs
            .iter()
            .map(|shown| shown.id)
            .filter(|id| !self.windows.values().any(|w| w.output == Some(*id)))
            .collect();
        for id in unused {
            let Some(at) = self.outputs.iter().position(|shown| shown.id == id) else {
                continue;
            };
            let shown = self.outputs.remove(at);
            self.display.disable_global::<State>(shown.global.clone());
            let _ = self.handle.insert_source(
                Timer::from_duration(OUTPUT_GRACE),
                move |_, _, state| {
                    state.display.remove_global::<State>(shown.global.clone());
                    TimeoutAction::Drop
                },
            );
        }
    }

    /// Read the selection GTK just set, in `types`, off this thread.
    fn read_selection(&mut self, seat: &Seat<State>, types: Vec<String>) {
        let mut reads = Vec::new();
        for ty in types {
            let Some((read, write)) = pipe() else {
                continue;
            };
            if request_data_device_client_selection(seat, ty.clone(), write).is_ok() {
                reads.push((ty, read));
            }
        }
        if reads.is_empty() {
            return;
        }
        let _ = self.display.flush_clients();
        let shared = self.shared.clone();
        // GTK writes the contents from its own thread once it sees the
        // request, so they are read on another thread than this one, which has
        // to keep serving GTK meanwhile.
        std::thread::spawn(move || {
            let entries: Vec<(String, Vec<u8>)> = reads
                .into_iter()
                .filter_map(|(ty, read)| {
                    let bytes = read_all(read);
                    if crate::engine::tracing() {
                        eprintln!(
                            "rosemary: webkit: compositor: read {ty}: {:?}",
                            bytes.as_ref().map(Vec::len)
                        );
                    }
                    Some((ty, bytes?))
                })
                .collect();
            if !entries.is_empty() {
                lock(&shared.events).push(Event::Selection { entries });
                (shared.wake)();
            }
        });
    }

    /// Where a popup of `window` may be: inside the window, which stands in
    /// for the screen, in the popup's parent's coordinates.
    fn constrain(&self, popup: &PopupKind, positioner: PositionerState) -> Rectangle<i32, Logical> {
        let Some(root) = find_popup_root_surface(popup).ok() else {
            return positioner.get_geometry();
        };
        let size = self
            .windows
            .get(&id(&root))
            .and_then(|w| w.configure)
            .map(|c| (c.width, c.height).into())
            .unwrap_or_else(|| geometry(&root).size);
        let mut target = Rectangle::from_size(size);
        target.loc -= get_popup_toplevel_coords(popup);
        positioner.get_unconstrained_geometry(target)
    }
}

/// A toplevel surface's window geometry, or its size when it sets none.
pub(super) fn geometry(surface: &WlSurface) -> Rectangle<i32, Logical> {
    with_states(surface, |states| {
        let geometry = states
            .cached_state
            .get::<SurfaceCachedState>()
            .current()
            .geometry;
        geometry.unwrap_or_else(|| {
            let size = states
                .data_map
                .get::<RendererSurfaceStateUserData>()
                .and_then(|data| data.lock().ok()?.surface_size())
                .unwrap_or_default();
            Rectangle::from_size(size)
        })
    })
}

/// One refresh at `millihertz`.
fn interval(millihertz: u32) -> Duration {
    Duration::from_nanos(1_000_000_000_000 / u64::from(millihertz.clamp(1_000, 1_000_000)))
}

fn set_scale(surface: &WlSurface, scale: f64) {
    with_states(surface, |states| {
        with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
        send_surface_state(surface, states, scale.ceil() as i32, Transform::Normal);
    });
}

fn has_callbacks(surface: &WlSurface) -> bool {
    let found = std::cell::Cell::new(false);
    with_surface_tree_downward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_, states, _| {
            let pending = !states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .is_empty();
            found.set(found.get() || pending);
        },
        |_, _, _| !found.get(),
    );
    found.get()
}

fn send_frames(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_, states, _| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, _| true,
    );
}

/// Where a surface's view is, and what part of its buffer it shows.
struct Placed {
    /// The buffer's pixels per logical pixel, horizontally.
    density: f64,
}

/// The window's surfaces and popups drawn into one picture, at the density
/// of its own buffer so that the common case is a plain copy.
fn compose(toplevel: &ToplevelSurface) -> Option<Frame> {
    let root = toplevel.wl_surface();
    let density = with_states(root, |states| {
        let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
        let data = data.lock().ok()?;
        let view = data.view()?;
        (view.dst.w > 0)
            .then(|| view.src.size.w * f64::from(data.buffer_scale()) / f64::from(view.dst.w))
    })?;
    let placed = Placed { density };
    let geometry = geometry(root);
    let width = (f64::from(geometry.size.w) * density).round().max(1.0) as usize;
    let height = (f64::from(geometry.size.h) * density).round().max(1.0) as usize;
    let mut canvas = Canvas::new(width, height);
    draw_tree(&mut canvas, &placed, root, Point::default() - geometry.loc);
    for (popup, offset) in PopupManager::popups_for_surface(root) {
        draw_tree(
            &mut canvas,
            &placed,
            popup.wl_surface(),
            offset - popup.geometry().loc,
        );
    }
    Some(Frame {
        width: width as u32,
        height: height as u32,
        picture: appkit::backing::Picture::Pixels(canvas.pixels),
    })
}

fn draw_tree(
    canvas: &mut Canvas,
    placed: &Placed,
    surface: &WlSurface,
    origin: Point<i32, Logical>,
) {
    with_surface_tree_downward(
        surface,
        origin,
        |_, states, location| {
            let view = states
                .data_map
                .get::<RendererSurfaceStateUserData>()
                .and_then(|data| data.lock().ok()?.view());
            match view {
                Some(view) => TraversalAction::DoChildren(*location + view.offset),
                None => TraversalAction::SkipChildren,
            }
        },
        |_, states, location| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return;
            };
            let Ok(data) = data.lock() else {
                return;
            };
            let (Some(view), Some(buffer)) = (data.view(), data.buffer()) else {
                return;
            };
            let at = *location + view.offset;
            let scale = f64::from(data.buffer_scale());
            let from = Rect {
                x: (view.src.loc.x * scale).round() as i64,
                y: (view.src.loc.y * scale).round() as i64,
                width: (view.src.size.w * scale).round() as i64,
                height: (view.src.size.h * scale).round() as i64,
            };
            let to = Rect {
                x: (f64::from(at.x) * placed.density).round() as i64,
                y: (f64::from(at.y) * placed.density).round() as i64,
                width: (f64::from(view.dst.w) * placed.density).round() as i64,
                height: (f64::from(view.dst.h) * placed.density).round() as i64,
            };
            draw_buffer(canvas, buffer, from, to);
        },
        |_, _, _| true,
    );
}

fn draw_buffer(canvas: &mut Canvas, buffer: &wl_buffer::WlBuffer, from: Rect, to: Rect) {
    let _ = with_buffer_contents(buffer, |pointer, length, info| {
        let opaque = match info.format {
            Format::Argb8888 => false,
            Format::Xrgb8888 => true,
            _ => return,
        };
        let (width, height, stride) = (
            info.width.max(0) as usize,
            info.height.max(0) as usize,
            info.stride.max(0) as usize,
        );
        let offset = info.offset.max(0) as usize;
        if width == 0 || height == 0 || stride < width * 4 {
            return;
        }
        let needed = offset + (height - 1) * stride + width * 4;
        if needed > length {
            return;
        }
        // SAFETY: smithay maps the pool for the call's duration and `length`
        // is that mapping's size, which the bounds above stay within.
        let pixels = unsafe { std::slice::from_raw_parts(pointer.add(offset), needed - offset) };
        let source = composite::Source {
            pixels,
            width,
            height,
            stride,
            opaque,
        };
        composite::draw(canvas, &source, from, to);
    });
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("every client is inserted with this data")
            .compositor
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        // A dma-buf is drawn from only once the GPU has finished writing it,
        // for drivers that do not wait on their own.
        add_pre_commit_hook::<Self, _>(surface, |state, _, surface| {
            let dmabuf = with_states(surface, |states| {
                let mut attributes = states.cached_state.get::<SurfaceAttributes>();
                match attributes.pending().buffer.as_ref() {
                    Some(BufferAssignment::NewBuffer(buffer)) => get_dmabuf(buffer).cloned().ok(),
                    _ => None,
                }
            });
            let Some(dmabuf) = dmabuf else {
                return;
            };
            let Ok((blocker, source)) = dmabuf.generate_blocker(Interest::READ) else {
                return;
            };
            let Some(client) = surface.client() else {
                return;
            };
            let waiting = state.handle.insert_source(source, move |_, _, state| {
                let display = state.display.clone();
                state
                    .client_compositor_state(&client)
                    .blocker_cleared(state, &display);
                Ok(())
            });
            if waiting.is_ok() {
                add_blocker(surface, blocker);
            }
        });
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.popups.commit(surface);
        if let Some(window) = self.windows.get(&id(surface))
            && !window.toplevel.is_initial_configure_sent()
        {
            let toplevel = window.toplevel.clone();
            self.configure(id(surface));
            toplevel.send_configure();
        }
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
        {
            let _ = popup.send_configure();
        }
        if let Some(window) = self.window_of(surface)
            && let Some(entry) = self.windows.get_mut(&window)
        {
            entry.dirty = true;
            self.arm_frames(window);
        }
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = id(surface.wl_surface());
        self.output.enter(surface.wl_surface());
        let (configure, monitor) = self.early.remove(&window).unzip();
        self.windows.insert(
            window,
            Window {
                toplevel: surface,
                configure,
                monitor: monitor.flatten(),
                output: None,
                interval: interval(DEFAULT_REFRESH),
                released: Instant::now(),
                timer: false,
                pool: Pool::default(),
                dirty: false,
                created: Instant::now(),
                closed: false,
            },
        );
        self.place(window);
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let window = id(surface.wl_surface());
        if self
            .windows
            .get(&window)
            .is_some_and(|w| w.toplevel == surface)
        {
            self.windows.remove(&window);
            lock(&self.shared.frames).remove(&window);
            self.retire_outputs();
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let kind = PopupKind::Xdg(surface.clone());
        let geometry = self.constrain(&kind, positioner);
        surface.with_pending_state(|state| state.geometry = geometry);
        if let Err(why) = self.popups.track_popup(kind) {
            eprintln!("rosemary: webkit: compositor: a popup could not be tracked: {why:?}");
        }
        let scale = surface
            .get_parent_surface()
            .and_then(|parent| self.window_of(&parent))
            .and_then(|window| self.scale_of(window));
        if let Some(scale) = scale {
            set_scale(surface.wl_surface(), scale);
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        let geometry = self.constrain(&PopupKind::Xdg(surface.clone()), positioner);
        surface.with_pending_state(|state| {
            state.geometry = geometry;
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
        let _ = surface.send_pending_configure();
    }

    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        self.grabs.push(surface);
        self.focus_keyboard();
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        self.grabs.retain(|popup| *popup != surface);
        // The popup is gone from its window's picture, whichever window it was.
        for window in self.windows.values_mut() {
            window.dirty = true;
        }
        self.focus_keyboard();
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| surface.client());
        set_data_device_focus(&self.display, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        let Some(surface) = self.pointer_window else {
            return;
        };
        let name = match image {
            CursorImageStatus::Hidden => None,
            CursorImageStatus::Named(icon) => Some(icon.name()),
            // A cursor GTK draws itself; the nearest thing is the arrow.
            CursorImageStatus::Surface(_) => Some("default"),
        };
        lock(&self.shared.events).push(Event::Cursor { surface, name });
    }
}

impl SelectionHandler for State {
    type SelectionUserData = Arc<Vec<(String, Vec<u8>)>>;

    fn new_selection(
        &mut self,
        target: SelectionTarget,
        source: Option<SelectionSource>,
        seat: Seat<Self>,
    ) {
        if target != SelectionTarget::Clipboard {
            return;
        }
        let Some(source) = source else {
            return;
        };
        let offered = source.mime_types();
        if crate::engine::tracing() {
            eprintln!("rosemary: webkit: compositor: GTK offers {offered:?}");
        }
        let text = TEXT_TYPES
            .iter()
            .find(|ty| offered.iter().any(|o| o == *ty))
            .map(|ty| ty.to_string());
        let html = offered
            .iter()
            .any(|o| o == HTML_TYPE)
            .then(|| HTML_TYPE.to_string());
        let types: Vec<String> = [text, html].into_iter().flatten().collect();
        // The seat takes the selection on only once this returns, so it is
        // asked for its contents after that.
        self.handle
            .insert_idle(move |state| state.read_selection(&seat, types));
    }

    fn send_selection(
        &mut self,
        _target: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        entries: &Self::SelectionUserData,
    ) {
        let Some(bytes) = entries
            .iter()
            .find(|(ty, _)| *ty == mime_type)
            .map(|(_, bytes)| bytes.clone())
        else {
            return;
        };
        // GTK reads on its own thread; a large paste would fill the pipe and
        // stall this one.
        std::thread::spawn(move || {
            let _ = std::fs::File::from(fd).write_all(&bytes);
        });
    }
}

fn pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: a two-element array for the two descriptors.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return None;
    }
    // SAFETY: two new descriptors this call owns.
    Some(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Everything written to `fd` until its writer closes it, or `None` if that
/// takes too long.
fn read_all(fd: OwnedFd) -> Option<Vec<u8>> {
    const LIMIT: Duration = Duration::from_secs(2);
    let started = Instant::now();
    let mut file = std::fs::File::from(fd);
    let mut out = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let left = LIMIT.checked_sub(started.elapsed())?;
        let mut poll = libc::pollfd {
            fd: std::os::fd::AsRawFd::as_raw_fd(&file.as_fd()),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd for a descriptor this owns.
        let ready = unsafe { libc::poll(&mut poll, 1, left.as_millis() as i32) };
        if ready < 0 {
            return None;
        }
        if ready == 0 {
            continue;
        }
        match file.read(&mut chunk) {
            Ok(0) => return Some(out),
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        match self.gpu.as_mut().is_some_and(|gpu| gpu.import(&dmabuf)) {
            true => {
                let _ = notifier.successful::<State>();
            }
            false => notifier.failed(),
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }
}

impl OutputHandler for State {}

impl smithay::wayland::tablet_manager::TabletSeatHandler for State {}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        let scale = self
            .window_of(&surface)
            .and_then(|window| self.scale_of(window));
        if let Some(scale) = scale {
            set_scale(&surface, scale);
        }
    }
}

smithay::delegate_compositor!(State);
smithay::delegate_xdg_shell!(State);
smithay::delegate_shm!(State);
smithay::delegate_dmabuf!(State);
smithay::delegate_seat!(State);
smithay::delegate_data_device!(State);
smithay::delegate_output!(State);
smithay::delegate_viewporter!(State);
smithay::delegate_fractional_scale!(State);
smithay::delegate_cursor_shape!(State);
