//! The desktop's host: the one process that has the host window, the window
//! list and every window's picture, as a Mac's window server has them.
//!
//! Every guest of a prefix connects to it, so their windows share one
//! coordinate space, one stacking order and one pointer, whichever process
//! made them. It is started by the first guest that makes a window and goes a
//! few seconds after the last has left, like the process server.
//!
//! One thread does everything: it pumps SDL, answers guests, and draws. It
//! never waits on a guest - what a guest has not read is kept for it, and a
//! guest that stops reading altogether is dropped.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::CStr;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use server::codec;
use server::location;

use super::compose::{self, Buffer, Layer, Target};
use super::protocol::{self, CursorChoice, Message, Reply, Request, State, window_of};
use super::scene::{Scene, WindowId};
use super::shared::Shared;
use super::{Extent, lock_in, socket_in};
use crate::video::{self, Event, HitTestResult, flags};
use crate::{Cursor, Rect, Window};

const TITLE: &CStr = c"Rosemary desktop";

pub const DEFAULT_LINGER: Duration = Duration::from_secs(5);

/// How long a desktop nobody has joined yet waits for its first guest: one
/// started by hand comes up well before the program it was started for.
const FIRST_GUEST: Duration = Duration::from_secs(30);

/// How long the loop sleeps when no guest has anything to say: SDL's events
/// are only seen by asking, as in [`video::wait`].
const NAP: Duration = Duration::from_millis(2);

/// How long a guest has to say what a press landed on before the press is
/// taken as a plain click. Everybody's input waits behind it meanwhile, so
/// this is short: a guest that is busy must not hold the others' pointer.
const PROBE_PATIENCE: Duration = Duration::from_millis(500);

/// What a guest may leave unread before it is taken for dead.
const MAX_BACKLOG: usize = 8 << 20;

pub struct Options {
    /// Where the desktop listens: its prefix's rendezvous directory.
    pub directory: PathBuf,
    pub extent: Extent,
    /// How long the desktop outlives its last guest.
    pub linger: Duration,
}

#[derive(Debug)]
pub enum Outcome {
    Finished,
    AlreadyRunning,
}

type PeerId = u32;

/// One connected guest.
struct Peer {
    stream: UnixStream,
    unread: Vec<u8>,
    unsent: Vec<u8>,
    greeted: bool,
    /// The cursors it defined, by its own numbers.
    cursors: HashMap<u32, usize>,
    cursor: CursorChoice,
    cursor_hidden: bool,
    gone: bool,
}

/// What the host keeps for one window besides its place in the scene.
struct Surface {
    owner: PeerId,
    /// The picture last committed, which is what is shown.
    picture: Buffer,
    /// Where the guest draws the next one.
    shared: Option<Shared>,
    /// Whether the guest has committed a picture. Until then there is
    /// nothing to show.
    drawn: bool,
    hit_test: bool,
    /// What the owner was last told about it.
    told: Option<State>,
}

/// A left press waiting for its window's hit test.
struct Probing {
    peer: PeerId,
    serial: u32,
    button: u8,
    clicks: u8,
    since: Instant,
}

struct Host {
    directory: PathBuf,
    window: Window,
    window_id: u32,
    /// Host pixels per desktop point.
    density: f32,
    scene: Scene,
    surfaces: HashMap<WindowId, Surface>,
    peers: BTreeMap<PeerId, Peer>,
    last_window: WindowId,
    last_peer: PeerId,
    /// Host pixels to redraw besides what the scene reports.
    damage: Option<Rect>,
    /// The host surface's size when last drawn: SDL makes another when it
    /// changes, with none of the old one's pixels.
    drawn_size: (i32, i32),
    /// The window that asked for text input, which is the host's to give.
    text_input: Option<WindowId>,
    /// Likewise relative mouse mode, and whether the host is in it.
    relative: Option<WindowId>,
    host_relative: bool,
    /// Likewise a pointer rectangle, in the window's coordinates, a window
    /// that keeps the pointer in the whole of itself, and the rectangle the
    /// host has.
    confined: Option<(WindowId, Rect)>,
    grabbed: Option<WindowId>,
    host_confined: Option<Rect>,
    /// Whether the host shows its own desktop between the windows and takes
    /// the clicks made there, and the region it was last told to leave to
    /// this one.
    see_through: bool,
    input: Option<Vec<Rect>>,
    probe: Option<Probing>,
    last_serial: u32,
    /// What arrived for the host window while a press waited.
    held: VecDeque<Event>,
    /// What every guest was last told of the desktop and the modifiers.
    told_desktop: Option<((i32, i32), f32)>,
    told_modifiers: Option<u16>,
    /// Whose cursor the host shows, and SDL's own cursors already made.
    cursor_shown: Option<(Option<PeerId>, CursorChoice, bool)>,
    system_cursors: HashMap<i32, usize>,
    /// The host window was closed: every guest has been asked to quit.
    closed: bool,
    stop: bool,
}

fn union(damage: Option<Rect>, rect: Rect) -> Option<Rect> {
    match damage {
        Some(damage) => Some(damage.bounding(rect)),
        None => (!rect.is_empty()).then_some(rect),
    }
}

/// Take this directory's desktop lock and be its desktop until the last
/// guest has been gone for `linger`.
pub fn run(options: &Options) -> io::Result<Outcome> {
    location::prepare(&options.directory)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_in(&options.directory))?;
    // SAFETY: a valid descriptor this function owns for as long as it serves.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(Outcome::AlreadyRunning);
    }
    // This process draws the desktop; its own windows are not on one.
    super::HOSTING.store(true, std::sync::atomic::Ordering::Release);
    if !video::init_video() {
        return Err(io::Error::other("no video to show a desktop on"));
    }
    let mut host = Host::new(options)
        .ok_or_else(|| io::Error::other("the desktop's host window could not be made"))?;
    let socket = socket_in(&options.directory);
    // The lock is held, so whatever is still here is a dead desktop's.
    let _ = std::fs::remove_file(&socket);
    remove_stale_buffers(&options.directory);
    // Bound only now, so a guest that connects finds a desktop that is up.
    let listener = UnixListener::bind(&socket)?;
    listener.set_nonblocking(true)?;
    let result = host.serve(&listener, options.linger);
    // Unlinked before the lock is released, so a successor never removes a
    // socket another successor has just bound.
    let _ = std::fs::remove_file(&socket);
    drop(lock);
    result.map(|()| Outcome::Finished)
}

/// A guest that died between making a buffer and handing it over left its
/// file behind.
fn remove_stale_buffers(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().as_bytes().starts_with(b"buffer-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

impl Host {
    fn new(options: &Options) -> Option<Host> {
        let (width, height) = match options.extent {
            Extent::Sized(width, height) => (width, height),
            Extent::Fullscreen => {
                let bounds = video::native_display_bounds(video::primary_display())?;
                (bounds.w, bounds.h)
            }
        };
        // A desktop that takes the clicks made between its windows has to be
        // seen, so it is clear only where the host can be told not to.
        let see_through = video::has_window_input_regions();
        let mut host_flags = flags::HIGH_PIXEL_DENSITY;
        if see_through {
            host_flags |= flags::TRANSPARENT;
        }
        if options.extent == Extent::Fullscreen {
            host_flags |= flags::FULLSCREEN;
        }
        let window = video::create_native_window(TITLE, width, height, host_flags)?;
        let mut host = Host {
            directory: options.directory.clone(),
            window,
            window_id: video::window_id(window),
            density: video::window_pixel_density(window),
            scene: Scene::new((width, height)),
            surfaces: HashMap::new(),
            peers: BTreeMap::new(),
            last_window: 0,
            last_peer: 0,
            damage: None,
            drawn_size: (0, 0),
            text_input: None,
            relative: None,
            host_relative: false,
            confined: None,
            grabbed: None,
            host_confined: None,
            see_through,
            input: None,
            probe: None,
            last_serial: 0,
            held: VecDeque::new(),
            told_desktop: None,
            told_modifiers: None,
            cursor_shown: None,
            system_cursors: HashMap::new(),
            closed: false,
            stop: false,
        };
        host.damage_all();
        Some(host)
    }

    fn serve(&mut self, listener: &UnixListener, linger: Duration) -> io::Result<()> {
        let mut idle_since = Instant::now();
        let mut patience = FIRST_GUEST.max(linger);
        while !self.stop {
            while let Some(event) = video::poll() {
                self.take(event);
            }
            self.expire_probe();
            self.accept(listener);
            let peers: Vec<PeerId> = self.peers.keys().copied().collect();
            for peer in peers {
                self.read(peer);
            }
            self.sync_modifiers();
            self.sync_host_pointer();
            self.publish();
            self.flush();
            self.sync_input_region();
            self.sync_cursor();
            self.present();
            self.reap();
            match self.peers.is_empty() {
                true if idle_since.elapsed() >= patience => break,
                true => {}
                false => {
                    idle_since = Instant::now();
                    patience = linger;
                }
            }
            self.wait(listener);
        }
        Ok(())
    }

    /// Sleep until a guest says something or can be written to again, or
    /// for [`NAP`], after which SDL is asked again.
    fn wait(&self, listener: &UnixListener) {
        let mut fds = vec![libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        for peer in self.peers.values() {
            let writing = match peer.unsent.is_empty() {
                true => 0,
                false => libc::POLLOUT,
            };
            fds.push(libc::pollfd {
                fd: peer.stream.as_raw_fd(),
                events: libc::POLLIN | writing,
                revents: 0,
            });
        }
        // SAFETY: `fds` holds that many descriptors, each open in `self`.
        unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as libc::nfds_t,
                NAP.as_millis() as i32,
            )
        };
    }

    fn accept(&mut self, listener: &UnixListener) {
        while let Ok((stream, _)) = listener.accept() {
            // Whoever draws on this user's screen and reads their keys must
            // be this user, as the process server asks of its guests.
            let ours = starnix::socket::peer(&stream)
                // SAFETY: `getuid` has no preconditions.
                .is_some_and(|peer| peer.uid == unsafe { libc::getuid() });
            if !ours || stream.set_nonblocking(true).is_err() {
                continue;
            }
            self.last_peer += 1;
            self.peers.insert(
                self.last_peer,
                Peer {
                    stream,
                    unread: Vec::new(),
                    unsent: Vec::new(),
                    greeted: false,
                    cursors: HashMap::new(),
                    cursor: CursorChoice::Standard,
                    cursor_hidden: false,
                    gone: false,
                },
            );
        }
    }

    /// Take in what `id` has sent and answer every request in it.
    fn read(&mut self, id: PeerId) {
        let Some(peer) = self.peers.get_mut(&id) else {
            return;
        };
        let mut chunk = [0u8; 4096];
        loop {
            match peer.stream.read(&mut chunk) {
                Ok(0) => {
                    peer.gone = true;
                    break;
                }
                Ok(read) => peer.unread.extend_from_slice(&chunk[..read]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    peer.gone = true;
                    break;
                }
            }
        }
        loop {
            let Some(peer) = self.peers.get_mut(&id) else {
                return;
            };
            let request = match protocol::take_frame(&mut peer.unread) {
                Ok(Some(frame)) => codec::decode::<Request>(&frame),
                Ok(None) => return,
                Err(malformed) => Err(malformed),
            };
            let Ok(request) = request else {
                peer.gone = true;
                return;
            };
            let reply = self.answer(id, request);
            // Before the reply, so a guest with its answer has also seen
            // what came of the question.
            self.publish();
            self.send(id, &Message::Reply { reply });
        }
    }

    fn send(&mut self, id: PeerId, message: &Message) {
        if let Some(peer) = self.peers.get_mut(&id) {
            protocol::put_frame(&mut peer.unsent, message);
        }
    }

    fn broadcast(&mut self, message: &Message) {
        for peer in self.peers.values_mut().filter(|peer| peer.greeted) {
            protocol::put_frame(&mut peer.unsent, message);
        }
    }

    /// Write what the guests are owed, as far as each will take it now.
    fn flush(&mut self) {
        for peer in self.peers.values_mut() {
            while !peer.unsent.is_empty() && !peer.gone {
                match peer.stream.write(&peer.unsent) {
                    Ok(0) => peer.gone = true,
                    Ok(written) => {
                        peer.unsent.drain(..written);
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => peer.gone = true,
                }
            }
            if peer.unsent.len() > MAX_BACKLOG {
                peer.gone = true;
            }
        }
    }

    /// Forget the guests that left, and everything that was theirs.
    fn reap(&mut self) {
        let gone: Vec<PeerId> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.gone)
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            let windows: Vec<WindowId> = self
                .surfaces
                .iter()
                .filter(|(_, surface)| surface.owner == id)
                .map(|(window, _)| *window)
                .collect();
            for window in windows {
                self.remove_window(window);
            }
            if self.probe.as_ref().is_some_and(|probe| probe.peer == id) {
                self.finish_probe(HitTestResult::Normal);
            }
            // Its cursor may be the one showing.
            if let Some(peer) = self.peers.get_mut(&id) {
                peer.cursor = CursorChoice::Standard;
            }
            self.sync_cursor();
            if let Some(peer) = self.peers.remove(&id) {
                for cursor in peer.cursors.into_values() {
                    // SAFETY: made by `create_color_cursor` for this guest
                    // and no longer shown.
                    unsafe { video::destroy_cursor(cursor as Cursor) };
                }
            }
        }
    }

    fn remove_window(&mut self, id: WindowId) {
        self.scene.remove(id);
        self.surfaces.remove(&id);
        for holder in [&mut self.text_input, &mut self.relative, &mut self.grabbed] {
            if *holder == Some(id) {
                *holder = None;
            }
        }
        if self.confined.is_some_and(|(holder, _)| holder == id) {
            self.confined = None;
        }
    }

    fn owns(&self, peer: PeerId, window: WindowId) -> bool {
        self.surfaces
            .get(&window)
            .is_some_and(|surface| surface.owner == peer)
    }

    fn pixels(&self, points: i32) -> i32 {
        (points as f32 * self.density).round() as i32
    }

    /// The host pixels a rectangle of the desktop covers, and their
    /// neighbours a rounded edge may have touched.
    fn pixel_rect(&self, rect: Rect) -> Rect {
        let left = (rect.x as f32 * self.density).floor() as i32;
        let top = (rect.y as f32 * self.density).floor() as i32;
        let right = ((rect.x + rect.w) as f32 * self.density).ceil() as i32;
        let bottom = ((rect.y + rect.h) as f32 * self.density).ceil() as i32;
        Rect {
            x: left,
            y: top,
            w: right - left,
            h: bottom - top,
        }
    }

    /// Give a window's picture the size its frame now has.
    fn fit_picture(&mut self, id: WindowId) {
        let Some(frame) = self.scene.frame(id) else {
            return;
        };
        let (width, height) = (self.pixels(frame.w), self.pixels(frame.h));
        if let Some(surface) = self.surfaces.get_mut(&id) {
            surface.picture.resize(width, height);
        }
    }

    fn damage_all(&mut self) {
        self.damage = Some(self.pixel_rect(self.scene.bounds()));
    }

    fn state_of(&self, id: WindowId) -> Option<State> {
        let (x, y) = self.scene.position(id)?;
        Some(State {
            frame: self.scene.frame(id)?,
            x,
            y,
            flags: self.scene.flags(id)?,
            opacity: self.scene.opacity(id)?,
            parent: self.scene.parent(id),
            relative: self.host_relative && self.relative == Some(id),
            text_input: self.text_input == Some(id),
        })
    }

    /// Tell each guest what changed for it: the desktop, its windows, and
    /// the events the scene made, in that order, so that an event is read
    /// against the state it came of.
    fn publish(&mut self) {
        let desktop = (self.scene.size(), self.density);
        if self.told_desktop != Some(desktop) {
            self.told_desktop = Some(desktop);
            let message = self.desktop_message();
            self.broadcast(&message);
        }
        let windows: Vec<WindowId> = self.surfaces.keys().copied().collect();
        for window in windows {
            let (Some(state), Some(surface)) =
                (self.state_of(window), self.surfaces.get_mut(&window))
            else {
                continue;
            };
            if surface.told == Some(state) {
                continue;
            }
            surface.told = Some(state);
            let owner = surface.owner;
            self.fit_picture(window);
            self.send(owner, &Message::Window { window, state });
        }
        while let Some(event) = self.scene.take_event() {
            let about = window_of(&event);
            let message = Message::Event { event };
            match about {
                Some(window) => {
                    if let Some(owner) = self.surfaces.get(&window).map(|surface| surface.owner) {
                        self.send(owner, &message);
                    }
                }
                None => self.broadcast(&message),
            }
        }
    }

    fn desktop_message(&self) -> Message {
        let (width, height) = self.scene.size();
        Message::Desktop {
            width,
            height,
            density: self.density,
        }
    }

    fn sync_modifiers(&mut self) {
        let modifiers = video::keyboard_modifiers();
        if self.told_modifiers != Some(modifiers) {
            self.told_modifiers = Some(modifiers);
            self.broadcast(&Message::Modifiers { modifiers });
        }
    }

    fn answer(&mut self, peer: PeerId, request: Request) -> Reply {
        let done = |ok| Reply::Done { ok };
        let greeted = self.peers.get(&peer).is_some_and(|peer| peer.greeted);
        match (&request, greeted) {
            (Request::Hello { version, .. }, false) if *version == protocol::VERSION => {
                if let Some(peer) = self.peers.get_mut(&peer) {
                    peer.greeted = true;
                }
                let desktop = self.desktop_message();
                self.send(peer, &desktop);
                let modifiers = self.told_modifiers.unwrap_or_default();
                self.send(peer, &Message::Modifiers { modifiers });
                return Reply::Welcome;
            }
            (Request::Hello { .. }, _) | (_, false) => {
                return Reply::Refused {
                    why: "this desktop is another build's".into(),
                };
            }
            _ => {}
        }
        // A guest speaks for its own windows only.
        if request
            .window()
            .is_some_and(|window| !self.owns(peer, window))
        {
            return match request {
                Request::Create { .. } => Reply::Created { window: None },
                _ => done(false),
            };
        }
        match request {
            Request::Hello { .. } => done(false),
            Request::Create {
                width,
                height,
                flags: window_flags,
                parent,
            } => {
                let (parent, offset) = match parent {
                    Some(anchor) => (Some(anchor.window), (anchor.x, anchor.y)),
                    // Centred, where SDL puts a window not told otherwise.
                    None => {
                        let (desktop_width, desktop_height) = self.scene.size();
                        (
                            None,
                            ((desktop_width - width) / 2, (desktop_height - height) / 2),
                        )
                    }
                };
                self.last_window += 1;
                let window = self.last_window;
                self.surfaces.insert(
                    window,
                    Surface {
                        owner: peer,
                        picture: Buffer::default(),
                        shared: None,
                        drawn: false,
                        hit_test: false,
                        told: None,
                    },
                );
                self.scene
                    .add(window, parent, window_flags, offset, (width, height));
                Reply::Created {
                    window: Some(window),
                }
            }
            Request::Destroy { window } => {
                self.remove_window(window);
                done(true)
            }
            Request::SetSize {
                window,
                width,
                height,
            } => {
                self.scene.set_size(window, width, height);
                done(true)
            }
            Request::SetPosition { window, x, y } => {
                self.scene.set_position(window, x, y);
                done(true)
            }
            Request::Show { window } => {
                self.scene.show(window);
                done(true)
            }
            Request::Hide { window } => {
                self.scene.hide(window);
                done(true)
            }
            Request::Raise { window, focus } => {
                self.scene.raise(window, focus);
                done(true)
            }
            Request::SetCovering { window, state, on } => {
                self.scene.set_covering(window, state, on);
                done(true)
            }
            Request::SetOpacity { window, opacity } => {
                self.scene.set_opacity(window, opacity);
                done(true)
            }
            Request::SetFlag { window, flag, on } => {
                self.scene.set_flag(window, flag, on);
                done(true)
            }
            Request::SetMinimum {
                window,
                width,
                height,
            } => {
                self.scene.set_minimum(window, width, height);
                done(true)
            }
            Request::SetModalFor { window, parent } => {
                let ours = self.owns(peer, parent);
                if ours {
                    self.scene.set_modal_for(window, parent);
                }
                done(ours)
            }
            Request::SetLevel { window, level } => {
                self.scene.set_level(window, level);
                done(true)
            }
            Request::SetHitTest { window, on } => {
                if let Some(surface) = self.surfaces.get_mut(&window) {
                    surface.hit_test = on;
                }
                done(true)
            }
            Request::Attach {
                window,
                name,
                width,
                height,
            } => done(self.attach(window, &name, width, height)),
            Request::Commit { window } => done(self.commit(window)),
            Request::Pointer => {
                let (x, y) = self.scene.pointer();
                Reply::Pointer {
                    x,
                    y,
                    buttons: self.scene.buttons(),
                    over: self
                        .scene
                        .pointer_window()
                        .filter(|window| self.owns(peer, *window)),
                }
            }
            Request::Warp { x, y } => done(match video::host_is_wayland() {
                // What SDL can do only where the compositor has a protocol.
                true => crate::pointer::warp(self.window, x, y).is_some(),
                false => video::warp_mouse_in_window(self.window, x, y),
            }),
            Request::SetRelative { window, on } => {
                match on {
                    true => self.relative = Some(window),
                    false if self.relative == Some(window) => self.relative = None,
                    false => {}
                }
                self.sync_host_pointer();
                done(true)
            }
            Request::SetMouseRect { window, rect } => {
                match rect {
                    Some(rect) => self.confined = Some((window, rect)),
                    None if self.confined.is_some_and(|(holder, _)| holder == window) => {
                        self.confined = None;
                    }
                    None => {}
                }
                self.sync_host_pointer();
                done(true)
            }
            Request::SetGrab { window, on } => {
                match on {
                    true => self.grabbed = Some(window),
                    false if self.grabbed == Some(window) => self.grabbed = None,
                    false => {}
                }
                self.sync_host_pointer();
                done(true)
            }
            Request::StartTextInput { window } => {
                let started = video::start_text_input(self.window);
                if started {
                    self.text_input = Some(window);
                }
                done(started)
            }
            Request::StopTextInput { window } => {
                let ours = self
                    .text_input
                    .take_if(|holder| *holder == window)
                    .is_some();
                done(ours && video::stop_text_input(self.window))
            }
            Request::SetTextInputArea {
                window,
                area,
                cursor,
            } => {
                let frame = self.scene.frame(window).unwrap_or_default();
                let area = Rect {
                    x: frame.x + area.x,
                    y: frame.y + area.y,
                    ..area
                };
                done(video::set_text_input_area(self.window, area, cursor))
            }
            Request::DefineCursor {
                cursor,
                width,
                height,
                hot_x,
                hot_y,
                pixels,
            } => {
                let made = video::create_color_cursor(width, height, &pixels, hot_x, hot_y);
                if let (Some(made), Some(peer)) = (made, self.peers.get_mut(&peer)) {
                    peer.cursors.insert(cursor, made as usize);
                }
                done(made.is_some())
            }
            Request::ForgetCursor { cursor } => {
                let choice = CursorChoice::Defined { cursor };
                if let Some(peer) = self.peers.get_mut(&peer)
                    && peer.cursor == choice
                {
                    peer.cursor = CursorChoice::Standard;
                }
                self.sync_cursor();
                let forgotten = self
                    .peers
                    .get_mut(&peer)
                    .and_then(|peer| peer.cursors.remove(&cursor));
                if let Some(forgotten) = forgotten {
                    // SAFETY: made by `create_color_cursor` for this guest
                    // and, as of the lines above, not the one shown.
                    unsafe { video::destroy_cursor(forgotten as Cursor) };
                }
                done(forgotten.is_some())
            }
            Request::SetCursor { choice } => {
                let Some(peer) = self.peers.get_mut(&peer) else {
                    return done(false);
                };
                let known = match choice {
                    CursorChoice::Defined { cursor } => peer.cursors.contains_key(&cursor),
                    _ => true,
                };
                if known {
                    peer.cursor = choice;
                }
                done(known)
            }
            Request::SetCursorVisible { visible } => {
                if let Some(peer) = self.peers.get_mut(&peer) {
                    peer.cursor_hidden = !visible;
                }
                done(true)
            }
            Request::SetClipboard { text } => done(crate::clipboard::set_text(&text)),
            Request::Clipboard => Reply::Clipboard {
                text: crate::clipboard::text(),
            },
            Request::Probed { serial, hit } => {
                let ours = self
                    .probe
                    .as_ref()
                    .is_some_and(|probe| probe.peer == peer && probe.serial == serial);
                if ours {
                    self.finish_probe(hit);
                }
                done(ours)
            }
            #[cfg(test)]
            Request::Inject { event } => {
                self.take(protocol::about(event, self.window_id));
                done(true)
            }
            #[cfg(test)]
            Request::Shown { x, y } => {
                self.present();
                let mut pixel = [0; 4];
                video::present_native(self.window, None, |surface| {
                    let at = y as usize * surface.pitch as usize + x as usize * 4;
                    // SAFETY: a point inside the surface SDL lent for the call.
                    pixel.copy_from_slice(unsafe {
                        std::slice::from_raw_parts(surface.pixels.add(at), 4)
                    });
                    false
                });
                Reply::Pixel {
                    blue: pixel[0],
                    green: pixel[1],
                    red: pixel[2],
                    alpha: pixel[3],
                }
            }
        }
    }

    /// Map the pixels a guest made for `window`. The file is the guest's,
    /// which is this user: one that cut it short under the mapping would
    /// fault this process, as it could by other means.
    fn attach(&mut self, window: WindowId, name: &[u8], width: i32, height: i32) -> bool {
        // A name in the desktop's own directory, and nowhere else.
        if name.is_empty() || name.contains(&b'/') || !name.starts_with(b"buffer-") {
            return false;
        }
        let path = self.directory.join(std::ffi::OsStr::from_bytes(name));
        let Ok(shared) = Shared::open(&path, width, height) else {
            return false;
        };
        match self.surfaces.get_mut(&window) {
            Some(surface) => {
                surface.shared = Some(shared);
                true
            }
            None => false,
        }
    }

    /// Take the guest's pixels as the window's picture. They are copied, so
    /// that what is shown is a finished picture whatever the guest draws next.
    fn commit(&mut self, window: WindowId) -> bool {
        let Some(frame) = self.scene.frame(window) else {
            return false;
        };
        let damage = self.pixel_rect(frame);
        let Some(surface) = self.surfaces.get_mut(&window) else {
            return false;
        };
        let Some(shared) = &surface.shared else {
            return false;
        };
        // A guest that has not caught up with a resize still shows what it
        // has, from the top-left, as a window resized before it redraws does.
        let bytes = surface.picture.pitch().min(shared.pitch());
        let (to_pitch, from_pitch) = (surface.picture.pitch(), shared.pitch());
        for row in 0..surface.picture.height.min(shared.height).max(0) as usize {
            let source = &shared.pixels()[row * from_pitch..row * from_pitch + bytes];
            surface.picture.pixels[row * to_pitch..row * to_pitch + bytes].copy_from_slice(source);
        }
        surface.drawn = true;
        self.damage = union(self.damage, damage);
        true
    }

    /// Take an event SDL delivered. One about the host window becomes events
    /// for the windows on the desktop.
    fn take(&mut self, event: Event) {
        if window_of(&event) != Some(self.window_id) {
            // SDL quits when its last window is closed, which has been
            // passed on already; any other quit is this process being ended.
            if event == Event::Quit && !self.closed {
                self.stop = true;
            }
            return;
        }
        match self.probe {
            Some(_) => self.held.push_back(event),
            None => self.route(event),
        }
    }

    fn route(&mut self, event: Event) {
        match event {
            Event::MouseMotion { x, y, dx, dy, .. } => match self.relative {
                // The pointer stays where it is and only its deltas arrive.
                Some(window) if self.host_relative => {
                    let (pointer, origin) = (
                        self.scene.pointer(),
                        self.scene.frame(window).unwrap_or_default(),
                    );
                    self.scene.push(Event::MouseMotion {
                        window,
                        x: pointer.0 - origin.x as f32,
                        y: pointer.1 - origin.y as f32,
                        dx,
                        dy,
                        buttons: self.scene.buttons(),
                    });
                }
                _ => self.scene.pointer_moved(x, y, dx, dy),
            },
            Event::MouseButton {
                button,
                down,
                clicks,
                x,
                y,
                ..
            } => {
                if !self.host_relative && self.scene.pointer() != (x, y) {
                    self.scene.pointer_moved(x, y, 0.0, 0.0);
                }
                let asked = (down && button == crate::mouse::LEFT)
                    .then(|| self.scene.press_point())
                    .flatten()
                    .and_then(|(window, point)| {
                        let surface = self.surfaces.get(&window)?;
                        surface.hit_test.then_some((surface.owner, window, point))
                    });
                match asked {
                    // The hit test is the guest's to run: the press waits.
                    Some((peer, window, (x, y))) => {
                        self.last_serial = self.last_serial.wrapping_add(1);
                        let serial = self.last_serial;
                        self.probe = Some(Probing {
                            peer,
                            serial,
                            button,
                            clicks,
                            since: Instant::now(),
                        });
                        self.send(
                            peer,
                            &Message::Probe {
                                serial,
                                window,
                                x,
                                y,
                            },
                        );
                    }
                    None => self
                        .scene
                        .button(button, down, clicks, HitTestResult::Normal),
                }
            }
            Event::MouseWheel { x, y, .. } => {
                if let Some(window) = self.scene.pointer_window() {
                    self.scene.push(Event::MouseWheel { window, x, y });
                }
            }
            Event::Pinch { scale, phase, .. } => {
                if let Some(window) = self.scene.pointer_window() {
                    self.scene.push(Event::Pinch {
                        window,
                        scale,
                        phase,
                    });
                }
            }
            Event::Key {
                scancode,
                key,
                modifiers,
                down,
                repeat,
                ..
            } => {
                let window = self.scene.keyboard();
                self.scene.push(Event::Key {
                    window,
                    scancode,
                    key,
                    modifiers,
                    down,
                    repeat,
                });
            }
            Event::Text { text, .. } => {
                let window = self.scene.keyboard();
                self.scene.push(Event::Text { window, text });
            }
            Event::TextEditing {
                text,
                start,
                length,
                ..
            } => {
                let window = self.scene.keyboard();
                self.scene.push(Event::TextEditing {
                    window,
                    text,
                    start,
                    length,
                });
            }
            Event::WindowFocus { gained, .. } => self.scene.host_focus(gained),
            Event::WindowMouse { entered: false, .. } => self.scene.pointer_left(),
            Event::WindowResized { .. }
            | Event::WindowBackingChanged { .. }
            | Event::WindowDisplayChanged { .. } => self.resync(),
            // Closing the desktop closes the session, as closing a screen
            // would: every guest is asked to quit.
            Event::WindowClosed { .. } => {
                self.closed = true;
                self.scene.push(Event::Quit);
            }
            _ => {}
        }
    }

    /// The press that waited lands, as `hit` says, and what queued behind it
    /// follows.
    fn finish_probe(&mut self, hit: HitTestResult) {
        let Some(probe) = self.probe.take() else {
            return;
        };
        self.scene.button(probe.button, true, probe.clicks, hit);
        while self.probe.is_none()
            && let Some(event) = self.held.pop_front()
        {
            self.route(event);
        }
    }

    fn expire_probe(&mut self) {
        if self
            .probe
            .as_ref()
            .is_some_and(|probe| probe.since.elapsed() >= PROBE_PATIENCE)
        {
            self.finish_probe(HitTestResult::Normal);
        }
    }

    /// The pointer rectangle the host should have: the one a window asked
    /// for, for as long as that window has the keyboard.
    fn wanted_confinement(&self) -> Option<Rect> {
        let focus = self.scene.keyboard();
        let frame = self.scene.frame(focus)?;
        match self.confined {
            Some((id, rect)) if id == focus => Some(Rect {
                x: frame.x + rect.x,
                y: frame.y + rect.y,
                ..rect
            }),
            _ => (self.grabbed == Some(focus)).then_some(frame),
        }
    }

    /// Bring the host's relative mode and pointer rectangle to what the
    /// window with the keyboard asked for: SDL holds both for a window only
    /// while it has the focus, and here the host window always has it.
    fn sync_host_pointer(&mut self) {
        if self.relative.is_some_and(|id| self.scene.keyboard() != id) {
            self.relative = None;
        }
        let relative = self.relative.is_some();
        if relative != self.host_relative {
            self.host_relative = relative;
            video::set_relative_mouse_mode(self.window, relative);
        }
        let confined = self.wanted_confinement();
        if confined != self.host_confined {
            self.host_confined = confined;
            video::set_mouse_rect(self.window, confined);
        }
    }

    /// Leave the host everything between the windows now showing: a click
    /// there is for whatever it has behind the desktop.
    fn sync_input_region(&mut self) {
        // While a button is held the host sends the pointer here wherever it
        // goes, so a window being dragged is followed once it is let go.
        if !self.see_through || self.scene.buttons() != 0 {
            return;
        }
        let region = self.scene.pointer_region();
        if self.input.as_ref() != Some(&region)
            && video::set_window_input_region(self.window, &region)
        {
            self.input = Some(region);
        }
    }

    /// Show the cursor of the guest whose window the pointer is over, as a
    /// Mac shows the cursor of the application under it.
    fn sync_cursor(&mut self) {
        let owner = self
            .scene
            .pointer_window()
            .and_then(|window| self.surfaces.get(&window))
            .map(|surface| surface.owner);
        let (choice, hidden) = owner
            .and_then(|owner| self.peers.get(&owner))
            .map_or((CursorChoice::Standard, false), |peer| {
                (peer.cursor, peer.cursor_hidden)
            });
        // One guest's cursor numbers are not another's.
        let whose = owner.filter(|_| matches!(choice, CursorChoice::Defined { .. }));
        let wanted = Some((whose, choice, hidden));
        if wanted == self.cursor_shown {
            return;
        }
        self.cursor_shown = wanted;
        let cursor = match choice {
            CursorChoice::Standard => None,
            CursorChoice::System { id } => match self.system_cursors.get(&id) {
                Some(cursor) => Some(*cursor),
                None => {
                    let made = video::create_system_cursor(id).map(|cursor| cursor as usize);
                    if let Some(made) = made {
                        self.system_cursors.insert(id, made);
                    }
                    made
                }
            },
            CursorChoice::Defined { cursor } => owner
                .and_then(|owner| self.peers.get(&owner))
                .and_then(|peer| peer.cursors.get(&cursor))
                .copied(),
        };
        video::set_cursor(cursor.map_or(std::ptr::null_mut(), |cursor| cursor as Cursor));
        video::set_cursor_visible(!hidden);
    }

    /// The host window changed size or scale: the desktop's are the host's.
    fn resync(&mut self) {
        let size = video::window_size(self.window);
        let density = video::window_pixel_density(self.window);
        if let Some(size) = size.filter(|size| *size != self.scene.size()) {
            self.scene.resize(size);
            self.scene.push(Event::DisplayModeChanged {
                display: video::primary_display(),
            });
        }
        if density != self.density {
            self.density = density;
            let windows: Vec<WindowId> = self.surfaces.keys().copied().collect();
            for window in windows {
                self.scene.push(Event::WindowBackingChanged { window });
                self.fit_picture(window);
            }
        }
        self.damage_all();
    }

    /// Show what changed since the last call.
    fn present(&mut self) {
        let changed = self.scene.take_damage().map(|rect| self.pixel_rect(rect));
        let damage = match (self.damage.take(), changed) {
            (Some(damage), Some(changed)) => damage.bounding(changed),
            (damage, changed) => match damage.or(changed) {
                Some(damage) => damage,
                None => return,
            },
        };
        let layers: Vec<Layer> = self
            .scene
            .stack()
            .into_iter()
            .filter_map(|id| {
                let frame = self.scene.frame(id)?;
                let surface = self.surfaces.get(&id).filter(|surface| surface.drawn)?;
                Some(Layer {
                    buffer: &surface.picture,
                    x: self.pixels(frame.x),
                    y: self.pixels(frame.y),
                    alpha: self.scene.flags(id)? & flags::TRANSPARENT != 0,
                    opacity: self.scene.opacity(id)?,
                })
            })
            .collect();
        let background = match self.see_through {
            true => compose::CLEAR,
            false => compose::BACKGROUND,
        };
        let drawn_size = self.drawn_size;
        let mut size = drawn_size;
        video::present_native(self.window, Some(damage), |surface| {
            size = (surface.width, surface.height);
            let whole = Rect {
                x: 0,
                y: 0,
                w: surface.width,
                h: surface.height,
            };
            let damage = match size == drawn_size {
                true => damage,
                false => whole,
            };
            // SAFETY: SDL owns `pitch * height` bytes for the length of the
            // call.
            let pixels = unsafe {
                std::slice::from_raw_parts_mut(
                    surface.pixels,
                    surface.pitch.max(0) as usize * surface.height.max(0) as usize,
                )
            };
            let mut target = Target {
                pixels,
                pitch: surface.pitch.max(0) as usize,
                width: surface.width,
                height: surface.height,
            };
            compose::compose(&mut target, background, &layers, damage);
            true
        });
        self.drawn_size = size;
    }
}
