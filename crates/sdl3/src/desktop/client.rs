//! A guest process's side of the desktop.
//!
//! One connection per process, opened by the first window it makes. Every
//! change to a window is asked of the host and waited for; everything read
//! back - a frame, flags, the desktop's size - is what the host last said,
//! which by then includes what came of the asking (see [`super::protocol`]).
//!
//! Two locks, taken in this order when both are: the connection's, held for
//! the length of one question, and the table's, held only to look something
//! up. Nothing here calls out with either held.

use std::collections::{HashMap, VecDeque};
use std::ffi::CStr;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use server::codec;

use super::protocol::{
    self, Anchor, CursorChoice, Message, Reply, Request, State, about, window_of,
};
use super::scene::WindowId;
use super::shared::Shared;
use super::{CONFIG, Config, ENV, Extent, extent, refuse, socket_in};
use crate::video::{self, Event, HitTest, HitTestResult, Point, Presentation, flags};
use crate::{Cursor, Rect, Window};

/// How long a guest waits for a desktop it launched to start listening.
const STARTUP: Duration = Duration::from_secs(5);

/// How long a guest waits for an answer before it takes the desktop for
/// dead: a host that stops answering must not hang every guest forever.
const PATIENCE: Duration = Duration::from_secs(10);

/// One connection to a desktop's host.
pub(crate) struct Connection {
    stream: UnixStream,
    unread: Vec<u8>,
}

impl Connection {
    /// Connect and say hello. `other` takes what the host says first: the
    /// desktop's size and the modifiers held.
    pub(crate) fn open(socket: &Path, other: &mut dyn FnMut(Message)) -> io::Result<Connection> {
        let stream = UnixStream::connect(socket)?;
        // The desktop reads this user's keys and draws on their screen, so
        // whoever listens must be this user, as with the process server.
        let ours = starnix::socket::peer(&stream)
            // SAFETY: `getuid` has no preconditions.
            .is_some_and(|peer| peer.uid == unsafe { libc::getuid() });
        if !ours {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is served by another user", socket.display()),
            ));
        }
        stream.set_read_timeout(Some(PATIENCE))?;
        let mut connection = Connection {
            stream,
            unread: Vec::new(),
        };
        let hello = Request::Hello {
            version: protocol::VERSION,
            pid: std::process::id() as i32,
        };
        match connection.call(&hello, other)? {
            Reply::Welcome => Ok(connection),
            Reply::Refused { why } => Err(io::Error::other(why)),
            _ => Err(io::Error::other("unexpected reply to a hello")),
        }
    }

    /// Ask, and wait for the answer. `other` takes everything the host sent
    /// before it.
    pub(crate) fn call(
        &mut self,
        request: &Request,
        other: &mut dyn FnMut(Message),
    ) -> io::Result<Reply> {
        let mut frame = Vec::new();
        protocol::put_frame(&mut frame, request);
        self.stream.write_all(&frame)?;
        loop {
            match self.next(true)? {
                Some(Message::Reply { reply }) => return Ok(reply),
                Some(message) => other(message),
                None => return Err(io::ErrorKind::UnexpectedEof.into()),
            }
        }
    }

    /// Hand `other` whatever has arrived, without waiting for more.
    pub(crate) fn drain(&mut self, other: &mut dyn FnMut(Message)) -> io::Result<()> {
        while let Some(message) = self.next(false)? {
            other(message);
        }
        Ok(())
    }

    /// The next message; `None` only when not waiting and none is here.
    fn next(&mut self, wait: bool) -> io::Result<Option<Message>> {
        loop {
            if let Some(frame) = protocol::take_frame(&mut self.unread)? {
                return Ok(Some(codec::decode(&frame)?));
            }
            let mut chunk = [0u8; 4096];
            let flags = match wait {
                true => 0,
                false => libc::MSG_DONTWAIT,
            };
            // SAFETY: a live socket and a writable buffer of that length.
            let read = unsafe {
                libc::recv(
                    self.stream.as_raw_fd(),
                    chunk.as_mut_ptr().cast(),
                    chunk.len(),
                    flags,
                )
            };
            match read {
                0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                read if read > 0 => self.unread.extend_from_slice(&chunk[..read as usize]),
                _ => match io::Error::last_os_error() {
                    e if e.kind() == io::ErrorKind::Interrupted => {}
                    e if e.kind() == io::ErrorKind::WouldBlock && !wait => return Ok(None),
                    e => return Err(e),
                },
            }
        }
    }
}

/// A window of this process's on the desktop.
struct Carrier {
    /// The SDL window that stands for it here and is never shown.
    handle: usize,
    sdl_id: u32,
    /// The host's number for it.
    id: WindowId,
    hit_test: Option<HitTest>,
    /// Where it is drawn; gone while a draw has it.
    buffer: Option<Shared>,
    drawing: bool,
    mouse_rect: Option<Rect>,
}

/// What the host sent that the video thread has yet to take.
enum Item {
    Event(Event),
    Probe {
        serial: u32,
        window: WindowId,
        x: i32,
        y: i32,
    },
}

/// A cursor this process made, as the host needs it described.
enum CursorKind {
    System(i32),
    Colour {
        number: u32,
        width: i32,
        height: i32,
        hot_x: i32,
        hot_y: i32,
        pixels: Vec<u8>,
        /// Whether the host has it yet: it is sent when first shown.
        sent: bool,
    },
}

#[derive(Default)]
struct Client {
    carriers: Vec<Carrier>,
    /// What the host last said of each window.
    states: HashMap<WindowId, State>,
    queue: VecDeque<Item>,
    size: (i32, i32),
    density: f32,
    modifiers: u16,
    cursors: HashMap<usize, CursorKind>,
    last_cursor: u32,
    /// The cursor this process wants shown, null for the host's own.
    cursor: usize,
    cursor_hidden: bool,
}

impl Client {
    fn carrier(&self, window: Window) -> Option<&Carrier> {
        self.carriers
            .iter()
            .find(|carrier| carrier.handle == window as usize)
    }

    fn carrier_mut(&mut self, window: Window) -> Option<&mut Carrier> {
        self.carriers
            .iter_mut()
            .find(|carrier| carrier.handle == window as usize)
    }

    fn pixels(&self, points: i32) -> i32 {
        (points as f32 * self.density).round() as i32
    }
}

static CLIENT: LazyLock<Mutex<Client>> = LazyLock::new(Mutex::default);

struct Link {
    connection: Connection,
    /// The process that opened it; a `fork` child holds its parent's.
    pid: u32,
}

static LINK: Mutex<Option<Link>> = Mutex::new(None);

/// Whether this process has a desktop now, and whether it ever had one or
/// was refused one: checked before either lock on paths every window call
/// and every event poll takes.
static UP: AtomicBool = AtomicBool::new(false);
static JOINED: AtomicBool = AtomicBool::new(false);
static UNAVAILABLE: AtomicBool = AtomicBool::new(false);

fn client() -> MutexGuard<'static, Client> {
    CLIENT.lock().unwrap_or_else(|e| e.into_inner())
}

fn link() -> MutexGuard<'static, Option<Link>> {
    LINK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Take what the host sent besides an answer.
fn apply(message: Message) {
    let mut client = client();
    match message {
        Message::Desktop {
            width,
            height,
            density,
        } => {
            client.size = (width, height);
            client.density = density;
        }
        Message::Window { window, state } => {
            client.states.insert(window, state);
        }
        Message::Modifiers { modifiers } => client.modifiers = modifiers,
        Message::Event { event } => client.queue.push_back(Item::Event(event)),
        Message::Probe {
            serial,
            window,
            x,
            y,
        } => client.queue.push_back(Item::Probe {
            serial,
            window,
            x,
            y,
        }),
        // Every reply is taken by the call that asked.
        Message::Reply { .. } => {}
    }
}

/// The desktop is gone, and this process's windows with it: nothing is left
/// to show them on, which is the end of the session.
fn lost(why: &io::Error) {
    if UP.swap(false, Ordering::AcqRel) {
        eprintln!("rosemary: sdl3: the virtual desktop is gone ({why}); quitting");
        client().queue.push_back(Item::Event(Event::Quit));
    }
}

/// A child of `fork` has its parent's connection and table, and neither is
/// its own to use: it starts again with none.
fn disown(guard: &mut MutexGuard<'static, Option<Link>>) {
    if guard
        .as_ref()
        .is_some_and(|link| link.pid != std::process::id())
    {
        // Closes this process's copy of the descriptor only.
        **guard = None;
        UP.store(false, Ordering::Release);
        *client() = Client::default();
    }
}

fn call(request: &Request) -> Option<Reply> {
    let mut guard = link();
    disown(&mut guard);
    let link = guard.as_mut()?;
    match link.connection.call(request, &mut apply) {
        Ok(reply) => Some(reply),
        Err(e) => {
            *guard = None;
            drop(guard);
            lost(&e);
            None
        }
    }
}

fn done(request: Request) -> bool {
    matches!(call(&request), Some(Reply::Done { ok: true }))
}

/// Take in what the host has sent since the last question.
fn pump() {
    let mut guard = match LINK.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(e)) => e.into_inner(),
        // Somebody is asking, and takes in what arrives while they wait.
        Err(TryLockError::WouldBlock) => return,
    };
    disown(&mut guard);
    let Some(link) = guard.as_mut() else {
        return;
    };
    if let Err(e) = link.connection.drain(&mut apply) {
        *guard = None;
        drop(guard);
        lost(&e);
    }
}

fn open(config: &Config, extent: Extent) -> io::Result<Connection> {
    let socket = socket_in(&config.directory);
    match Connection::open(&socket, &mut apply) {
        Ok(connection) => return Ok(connection),
        Err(e) if config.launch.is_none() => return Err(e),
        Err(_) => {}
    }
    if let Some(launch) = config.launch {
        // The host's own refusal of the directory would be lost with its
        // stderr; checked here, the reason reaches the user.
        server::location::prepare(&config.directory)?;
        launch(&config.directory, extent)?;
    }
    let deadline = Instant::now() + STARTUP;
    let mut pause = Duration::from_millis(2);
    loop {
        match Connection::open(&socket, &mut apply) {
            Ok(connection) => return Ok(connection),
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(100));
            }
        }
    }
}

/// Join the prefix's desktop, starting it if nobody has.
fn join() -> bool {
    let mut guard = link();
    disown(&mut guard);
    if guard.is_some() {
        return true;
    }
    // One desktop for the life of the process: its windows cannot follow it
    // to another.
    if JOINED.load(Ordering::Acquire) || UNAVAILABLE.load(Ordering::Acquire) {
        return false;
    }
    let Some(extent) = extent() else {
        return false;
    };
    let joined = match CONFIG.get() {
        Some(config) => open(config, extent),
        None => Err(io::Error::other("this process was not told where it is")),
    };
    match joined {
        Ok(connection) => {
            *guard = Some(Link {
                connection,
                pid: std::process::id(),
            });
            JOINED.store(true, Ordering::Release);
            UP.store(true, Ordering::Release);
            drop(guard);
            // What this process asked for before it had a desktop.
            show_cursor();
            if client().cursor_hidden {
                done(Request::SetCursorVisible { visible: false });
            }
            true
        }
        Err(e) => {
            UNAVAILABLE.store(true, Ordering::Release);
            eprintln!(
                "rosemary: sdl3: no virtual desktop to make windows on ({ENV}): {e}; run \
                 `rosemary desktop --foreground` to see why it does not start"
            );
            false
        }
    }
}

fn id_of(window: Window) -> Option<WindowId> {
    client().carrier(window).map(|carrier| carrier.id)
}

fn state_of(window: Window) -> Option<State> {
    let client = client();
    let id = client.carrier(window)?.id;
    client.states.get(&id).copied()
}

/// The desktop's size before or after this process has joined it.
fn size() -> Option<(i32, i32)> {
    if UP.load(Ordering::Acquire) {
        return Some(client().size);
    }
    match extent()? {
        Extent::Sized(width, height) => Some((width, height)),
        Extent::Fullscreen => video::native_display_bounds(video::primary_display())
            .map(|bounds| (bounds.w, bounds.h)),
    }
}

/// The desktop as the one display there is, or `None` without a desktop.
pub(crate) fn bounds() -> Option<Rect> {
    if !super::active() {
        return None;
    }
    let (w, h) = size()?;
    Some(Rect { x: 0, y: 0, w, h })
}

/// A window on the desktop. `parent` and an offset from it make a popup,
/// placed as SDL places one.
pub(crate) fn create_window(
    title: &CStr,
    width: i32,
    height: i32,
    window_flags: u64,
    parent: Option<(Window, i32, i32)>,
) -> Option<Window> {
    if window_flags & flags::OPENGL != 0 {
        refuse("an OpenGL window");
        return None;
    }
    if !join() {
        return None;
    }
    let anchor = match parent {
        Some((parent, x, y)) => Some(Anchor {
            window: id_of(parent)?,
            x,
            y,
        }),
        None => None,
    };
    let window =
        video::create_native_window(title, width, height, flags::HIDDEN | flags::BORDERLESS)?;
    // SDL destroys a popup with its parent, and the caller counts on it.
    if let Some((parent, ..)) = parent
        && !video::set_native_parent(window, parent)
    {
        video::destroy_native_window(window);
        return None;
    }
    let created = call(&Request::Create {
        width,
        height,
        flags: window_flags,
        parent: anchor,
    });
    let Some(Reply::Created { window: Some(id) }) = created else {
        video::destroy_native_window(window);
        return None;
    };
    client().carriers.push(Carrier {
        handle: window as usize,
        sdl_id: video::window_id(window),
        id,
        hit_test: None,
        buffer: None,
        drawing: false,
        mouse_rect: None,
    });
    Some(window)
}

/// `window` is being destroyed.
pub(crate) fn destroyed(window: Window) {
    let id = {
        let mut client = client();
        let Some(id) = client.carrier(window).map(|carrier| carrier.id) else {
            return;
        };
        client
            .carriers
            .retain(|carrier| carrier.handle != window as usize);
        client.states.remove(&id);
        id
    };
    done(Request::Destroy { window: id });
}

/// Whether `window` is one of the desktop's.
pub(crate) fn owns(window: Window) -> bool {
    JOINED.load(Ordering::Acquire) && id_of(window).is_some()
}

pub(crate) fn window_size(window: Window) -> Option<(i32, i32)> {
    state_of(window).map(|state| (state.frame.w, state.frame.h))
}

pub(crate) fn set_window_size(window: Window, width: i32, height: i32) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::SetSize {
        window,
        width,
        height,
    }))
}

pub(crate) fn window_size_in_pixels(window: Window) -> Option<(i32, i32)> {
    let frame = state_of(window)?.frame;
    let client = client();
    Some((client.pixels(frame.w), client.pixels(frame.h)))
}

pub(crate) fn window_position(window: Window) -> Option<(i32, i32)> {
    state_of(window).map(|state| (state.x, state.y))
}

pub(crate) fn set_window_position(window: Window, x: i32, y: i32) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::SetPosition { window, x, y }))
}

pub(crate) fn window_flags(window: Window) -> Option<u64> {
    state_of(window).map(|state| state.flags)
}

pub(crate) fn window_pixel_density(window: Window) -> Option<f32> {
    let client = client();
    client.carrier(window).map(|_| client.density)
}

/// The window a popup hangs off, or null; `None` for a window not here.
pub(crate) fn window_parent(window: Window) -> Option<Window> {
    let parent = state_of(window)?.parent;
    let client = client();
    let carrier = client
        .carriers
        .iter()
        .find(|carrier| Some(carrier.id) == parent);
    Some(carrier.map_or(std::ptr::null_mut(), |carrier| carrier.handle as Window))
}

pub(crate) fn show_window(window: Window) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::Show { window }))
}

pub(crate) fn hide_window(window: Window) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::Hide { window }))
}

/// Bring a window to the front of its level, with the keyboard for `focus`.
pub(crate) fn raise_window(window: Window, focus: bool) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::Raise { window, focus }))
}

/// Enter or leave `state`, which is `FULLSCREEN` or `MAXIMIZED`.
pub(crate) fn set_window_covering(window: Window, state: u64, on: bool) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::SetCovering { window, state, on }))
}

pub(crate) fn set_window_opacity(window: Window, opacity: f32) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::SetOpacity { window, opacity }))
}

pub(crate) fn window_opacity(window: Window) -> Option<f32> {
    state_of(window).map(|state| state.opacity)
}

pub(crate) fn set_window_flag(window: Window, flag: u64, on: bool) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::SetFlag { window, flag, on }))
}

pub(crate) fn set_window_minimum_size(window: Window, width: i32, height: i32) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::SetMinimum {
        window,
        width,
        height,
    }))
}

pub(crate) fn set_window_hit_test(window: Window, callback: Option<HitTest>) -> Option<bool> {
    let id = {
        let mut client = client();
        let carrier = client.carrier_mut(window)?;
        carrier.hit_test = callback;
        carrier.id
    };
    Some(done(Request::SetHitTest {
        window: id,
        on: callback.is_some(),
    }))
}

/// Make `window` modal for `parent`, both on the desktop.
pub(crate) fn set_window_modal_for(window: Window, parent: Window) -> Option<bool> {
    let window = id_of(window)?;
    Some(match id_of(parent) {
        Some(parent) => done(Request::SetModalFor { window, parent }),
        None => false,
    })
}

/// Whether `window` is one a Vulkan surface is refused for, saying so.
pub(crate) fn refuses_surface(window: Window) -> bool {
    let refused = owns(window);
    if refused {
        refuse("a Vulkan surface");
    }
    refused
}

/// The level a window stacks at, as a Mac's window server is told it: an
/// `NSWindowLevel`, which is a `CGWindowLevel`. Without a desktop the host's
/// window manager stacks windows and has no such thing to be told.
pub fn set_window_level(window: Window, level: i32) {
    if let Some(window) = id_of(window) {
        done(Request::SetLevel { window, level });
    }
}

/// Make the pixels `id` is drawn into, of a size, keeping what `old` shows
/// of it, and give them to the host.
fn attach(id: WindowId, (width, height): (i32, i32), old: Option<Shared>) -> Option<Shared> {
    let directory = &CONFIG.get()?.directory;
    let (mut buffer, name) = Shared::create(directory, width, height).ok()?;
    if let Some(old) = &old {
        buffer.keep(old);
    }
    let attached = done(Request::Attach {
        window: id,
        name: name.as_os_str().as_bytes().to_vec(),
        width,
        height,
    });
    // The host has it open, or never will.
    let _ = std::fs::remove_file(directory.join(name));
    attached.then_some(buffer)
}

/// Let `body` draw `window`'s picture, as [`video::present_window`] does on
/// the window's surface. `Err` hands `body` back for a window not here.
pub(crate) fn draw<F>(window: Window, body: F) -> Result<bool, F>
where
    F: FnOnce(&Presentation) -> bool,
{
    // Out of the table while `body` runs: it asks about the window, which
    // takes the lock held here.
    let taken = {
        let mut client = client();
        let client = &mut *client;
        client
            .carriers
            .iter_mut()
            .find(|carrier| carrier.handle == window as usize)
            .map(|carrier| {
                let state = client.states.get(&carrier.id).copied();
                let free = state.filter(|_| !std::mem::replace(&mut carrier.drawing, true));
                free.map(|state| (carrier.id, carrier.buffer.take(), state))
            })
    };
    let Some(taken) = taken else {
        return Err(body);
    };
    // Somebody else is drawing it, and their picture is the one shown.
    let Some((id, buffer, state)) = taken else {
        return Ok(false);
    };
    let wanted = {
        let client = client();
        (client.pixels(state.frame.w), client.pixels(state.frame.h))
    };
    let buffer = match buffer {
        Some(buffer) if (buffer.width, buffer.height) == wanted => Some(buffer),
        old => attach(id, wanted, old),
    };
    let drew = buffer.is_some_and(|mut buffer| {
        let format = match state.flags & flags::TRANSPARENT != 0 {
            true => video::pixel_format::ARGB8888,
            false => video::pixel_format::XRGB8888,
        };
        let drew = body(&Presentation {
            width: buffer.width,
            height: buffer.height,
            pitch: buffer.pitch() as i32,
            format,
            pixels: buffer.pointer(),
        });
        if let Some(carrier) = client().carrier_mut(window) {
            carrier.buffer = Some(buffer);
        }
        drew
    });
    if let Some(carrier) = client().carrier_mut(window) {
        carrier.drawing = false;
    }
    if drew {
        done(Request::Commit { window: id });
    }
    Ok(drew)
}

/// `event` as this process knows its window, or `None` for one about a
/// window it no longer has.
fn local(event: Event) -> Option<Event> {
    match (window_of(&event), &event) {
        (Some(id), _) => {
            let client = client();
            let carrier = client.carriers.iter().find(|carrier| carrier.id == id)?;
            Some(about(event, carrier.sdl_id))
        }
        // The desktop is this process's one display, whatever SDL's is.
        (None, Event::DisplayModeChanged { .. }) => Some(Event::DisplayModeChanged {
            display: video::primary_display(),
        }),
        (None, _) => Some(event),
    }
}

/// Run a window's hit test on a point the host is about to press, and say
/// what it made of it.
fn probe(serial: u32, id: WindowId, x: i32, y: i32) {
    let asked = {
        let client = client();
        let carrier = client.carriers.iter().find(|carrier| carrier.id == id);
        carrier.and_then(|carrier| Some((carrier.hit_test?, carrier.handle)))
    };
    let hit = asked.map_or(HitTestResult::Normal, |(hit_test, handle)| {
        // SAFETY: the callback the window's owner registered, called as SDL
        // calls it: with its window and a live point.
        unsafe { hit_test(handle as Window, &Point { x, y }, std::ptr::null_mut()) }
    });
    done(Request::Probed { serial, hit });
}

/// The next event the desktop has for one of this process's windows.
pub(crate) fn take_event() -> Option<Event> {
    if !JOINED.load(Ordering::Acquire) {
        return None;
    }
    pump();
    loop {
        let item = client().queue.pop_front()?;
        match item {
            Item::Event(event) => {
                if let Some(event) = local(event) {
                    return Some(event);
                }
            }
            Item::Probe {
                serial,
                window,
                x,
                y,
            } => probe(serial, window, x, y),
        }
    }
}

/// The text of the next event, when the next event is text: what a key
/// produced follows it. `None` without a desktop.
pub(crate) fn take_next_text() -> Option<Option<String>> {
    if !UP.load(Ordering::Acquire) {
        return None;
    }
    pump();
    let mut client = client();
    Some(match client.queue.front() {
        Some(Item::Event(Event::Text { .. })) => match client.queue.pop_front() {
            Some(Item::Event(Event::Text { text, .. })) => Some(text),
            _ => None,
        },
        _ => None,
    })
}

/// Take an event SDL delivered. The desktop is the only one with anything to
/// say about its windows, so one about them goes no further.
pub(crate) fn route(event: Event) -> Option<Event> {
    if !JOINED.load(Ordering::Acquire) {
        return Some(event);
    }
    let ours = window_of(&event).is_some_and(|window| {
        client()
            .carriers
            .iter()
            .any(|carrier| carrier.sdl_id == window)
    });
    (!ours).then_some(event)
}

/// The pointer on the desktop, the held buttons, and the origin of the
/// window of this process's it is in, if it is in one.
fn pointer() -> Option<(f32, f32, u32, Option<(i32, i32)>)> {
    if !UP.load(Ordering::Acquire) {
        return None;
    }
    let Some(Reply::Pointer {
        x,
        y,
        buttons,
        over,
    }) = call(&Request::Pointer)
    else {
        return None;
    };
    let client = client();
    let origin = over
        .and_then(|window| client.states.get(&window))
        .map(|state| (state.frame.x, state.frame.y));
    Some((x, y, buttons, origin))
}

/// The pointer in the coordinates of the window it is in, or the desktop's
/// when it is in none of this process's, and the held buttons.
pub(crate) fn mouse_state() -> Option<(f32, f32, u32)> {
    let (x, y, buttons, origin) = pointer()?;
    let origin = origin.unwrap_or_default();
    Some((x - origin.0 as f32, y - origin.1 as f32, buttons))
}

pub(crate) fn global_mouse_state() -> Option<(f32, f32, u32)> {
    let (x, y, buttons, _) = pointer()?;
    Some((x, y, buttons))
}

/// Move the pointer to a desktop point, which is a point of the host window.
pub(crate) fn warp_mouse_global(x: f32, y: f32) -> Option<bool> {
    UP.load(Ordering::Acquire)
        .then(|| done(Request::Warp { x, y }))
}

pub(crate) fn warp_mouse_in_window(window: Window, x: f32, y: f32) -> Option<bool> {
    let frame = state_of(window)?.frame;
    warp_mouse_global(frame.x as f32 + x, frame.y as f32 + y)
}

pub(crate) fn set_relative_mouse_mode(window: Window, on: bool) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::SetRelative { window, on }))
}

pub(crate) fn relative_mouse_mode(window: Window) -> Option<bool> {
    state_of(window).map(|state| state.relative)
}

pub(crate) fn set_mouse_rect(window: Window, rect: Option<Rect>) -> Option<bool> {
    let id = {
        let mut client = client();
        let carrier = client.carrier_mut(window)?;
        carrier.mouse_rect = rect;
        carrier.id
    };
    Some(done(Request::SetMouseRect { window: id, rect }))
}

pub(crate) fn set_mouse_grab(window: Window, on: bool) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::SetGrab { window, on }))
}

/// `Some(None)` is a window of the desktop's with no rectangle.
pub(crate) fn get_mouse_rect(window: Window) -> Option<Option<Rect>> {
    client().carrier(window).map(|carrier| carrier.mouse_rect)
}

pub(crate) fn start_text_input(window: Window) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::StartTextInput { window }))
}

pub(crate) fn stop_text_input(window: Window) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::StopTextInput { window }))
}

pub(crate) fn text_input_active(window: Window) -> Option<bool> {
    state_of(window).map(|state| state.text_input)
}

pub(crate) fn set_text_input_area(window: Window, area: Rect, cursor: i32) -> Option<bool> {
    let window = id_of(window)?;
    Some(done(Request::SetTextInputArea {
        window,
        area,
        cursor,
    }))
}

/// Which modifier keys are held, which only the host's window is told.
pub(crate) fn modifiers() -> Option<u16> {
    UP.load(Ordering::Acquire).then(|| client().modifiers)
}

/// The clipboard is the host window's to set and read: a window system
/// gives it to the program with the keyboard, which no guest's SDL has.
pub(crate) fn set_clipboard(text: &[u8]) -> Option<bool> {
    UP.load(Ordering::Acquire).then(|| {
        done(Request::SetClipboard {
            text: text.to_vec(),
        })
    })
}

/// `Some(None)` is a clipboard with no text.
pub(crate) fn clipboard() -> Option<Option<Vec<u8>>> {
    if !UP.load(Ordering::Acquire) {
        return None;
    }
    match call(&Request::Clipboard) {
        Some(Reply::Clipboard { text }) => Some(text),
        _ => Some(None),
    }
}

/// `cursor` is one of SDL's own, made for this process.
pub(crate) fn system_cursor_made(cursor: Cursor, id: i32) {
    if super::active() {
        client()
            .cursors
            .insert(cursor as usize, CursorKind::System(id));
    }
}

/// `cursor` was made of `width * height` RGBA pixels.
pub(crate) fn colour_cursor_made(
    cursor: Cursor,
    (width, height): (i32, i32),
    pixels: &[u8],
    (hot_x, hot_y): (i32, i32),
) {
    if !super::active() {
        return;
    }
    let mut client = client();
    client.last_cursor += 1;
    let kind = CursorKind::Colour {
        number: client.last_cursor,
        width,
        height,
        hot_x,
        hot_y,
        pixels: pixels.to_vec(),
        sent: false,
    };
    client.cursors.insert(cursor as usize, kind);
}

pub(crate) fn cursor_destroyed(cursor: Cursor) {
    let forgotten = {
        let mut client = client();
        if client.cursor == cursor as usize {
            client.cursor = 0;
        }
        client.cursors.remove(&(cursor as usize))
    };
    if let Some(CursorKind::Colour {
        number, sent: true, ..
    }) = forgotten
    {
        done(Request::ForgetCursor { cursor: number });
    }
}

/// Tell the host which cursor this process wants over its windows, sending
/// the cursor's picture the first time it is wanted.
fn show_cursor() -> bool {
    let (define, choice) = {
        let mut client = client();
        let wanted = client.cursor;
        match client.cursors.get_mut(&wanted) {
            None => (None, CursorChoice::Standard),
            Some(CursorKind::System(id)) => (None, CursorChoice::System { id: *id }),
            Some(CursorKind::Colour {
                number,
                width,
                height,
                hot_x,
                hot_y,
                pixels,
                sent,
            }) => {
                let define = (!std::mem::replace(sent, true)).then(|| Request::DefineCursor {
                    cursor: *number,
                    width: *width,
                    height: *height,
                    hot_x: *hot_x,
                    hot_y: *hot_y,
                    pixels: pixels.clone(),
                });
                (define, CursorChoice::Defined { cursor: *number })
            }
        }
    };
    if let Some(define) = define {
        done(define);
    }
    done(Request::SetCursor { choice })
}

/// Show `cursor` over this process's windows, or the host's own for null.
/// `None` without a desktop.
pub(crate) fn set_cursor(cursor: Cursor) -> Option<bool> {
    if !super::active() {
        return None;
    }
    client().cursor = cursor as usize;
    // Before this process has joined, it is shown when it does.
    Some(!UP.load(Ordering::Acquire) || show_cursor())
}

pub(crate) fn set_cursor_visible(visible: bool) -> Option<bool> {
    if !super::active() {
        return None;
    }
    client().cursor_hidden = !visible;
    Some(!UP.load(Ordering::Acquire) || done(Request::SetCursorVisible { visible }))
}

/// Ask the host something directly, for a test that needs what no window
/// call says.
#[cfg(test)]
pub(crate) fn ask(request: Request) -> Option<Reply> {
    call(&request)
}
