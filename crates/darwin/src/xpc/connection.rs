//! `xpc_connection_t`, over this crate's Mach ports.
//!
//! Every connection end owns one receive right, and holds a send right to the
//! other end's. That pairing is what tells each side the other is gone: a
//! process that exits takes its send right with it, and the survivor's port
//! reports no more senders.
//!
//! The handshake, by `msgh_id`:
//!
//! * [`id::HELLO`]: a client to a listener's port - the service's, or an
//!   endpoint's - carrying a send right to the client's own port as the reply
//!   port. The listener makes a peer connection and a port for it.
//! * [`id::WELCOME`]: the peer to the client, carrying a send right to the
//!   peer's port. Messages sent before it arrives wait for it.
//! * [`id::MESSAGE`]: a dictionary either way, or a reply to one, numbered by
//!   its sender so a reply finds the call that waits on it.
//!
//! One thread per process receives on a port set holding every connection's
//! port and hands each event to its connection, whose handler runs on the
//! connection's target queue, one event at a time.

use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use super::codec::{self, Reader};
use super::collection::{self, Dictionary, Origin};
use super::object::{self, Handle, Value, lock, new, new_global, release, retain};
use crate::dispatch::{block, queue};
use crate::mach::kern::*;
use crate::mach::space::{self, MACH_PORT_NULL, Name, RIGHT_PORT_SET, RIGHT_RECEIVE, TASK_SELF};
use crate::mach::{bootstrap, msg};

/// `XPC_CONNECTION_MACH_SERVICE_*`, the flags `xpc_connection_create_mach_service` takes.
const MACH_SERVICE_LISTENER: u64 = 1 << 0;

/// `MACH_NOTIFY_NO_SENDERS`, from `<mach/notify.h>`.
const NOTIFY_NO_SENDERS: i32 = 0o106;

/// The `msgh_id`s this transport sends. "XPC" and a number.
mod id {
    pub const HELLO: i32 = 0x5850_4301;
    pub const WELCOME: i32 = 0x5850_4302;
    pub const MESSAGE: i32 = 0x5850_4303;
    /// Sent to the receiving thread's own port when its port set changes.
    pub const WAKE: i32 = 0x5850_4304;
}

/// What a [`id::MESSAGE`] is, after its payload's first word.
mod kind {
    pub const REQUEST: u32 = 0;
    pub const REPLY: u32 = 1;
}

const WANTS_REPLY: u32 = 1 << 0;

/// `mach_msg_header_t` and the parts of a complex message, in bytes.
const HEADER: usize = 24;
const BODY: usize = 4;
const DESCRIPTOR: usize = 12;
/// `MACH_MSG_PORT_DESCRIPTOR`, and where a descriptor keeps its fields.
const PORT_DESCRIPTOR: u8 = 0;
const DESCRIPTOR_DISPOSITION_AT: usize = 10;
const DESCRIPTOR_TYPE_AT: usize = 11;
const BITS_COMPLEX: u32 = 0x8000_0000;
const BITS_LOCAL_SHIFT: u32 = 8;

/// `MACH_SEND_MSG`, `MACH_RCV_MSG`, `MACH_RCV_LARGE`, and the audit trailer:
/// `MACH_RCV_TRAILER_ELEMENTS(MACH_RCV_TRAILER_AUDIT)`.
const SEND_MSG: i32 = 0x1;
const RCV_MSG: i32 = 0x2;
const RCV_LARGE: i32 = 0x4;
const RCV_TRAILER_AUDIT: i32 = 3 << 24;
/// Where `audit_token_t` starts in `mach_msg_audit_trailer_t`.
const AUDIT_AT: usize = 20;
const AUDIT_TRAILER: usize = 52;

/// The three errors a connection reports, which Darwin keeps as globals.
pub fn error_interrupted() -> Handle {
    static ERROR: OnceLock<Handle> = OnceLock::new();
    *ERROR.get_or_init(|| error(c"Connection interrupted"))
}

pub fn error_invalid() -> Handle {
    static ERROR: OnceLock<Handle> = OnceLock::new();
    *ERROR.get_or_init(|| error(c"Connection invalid"))
}

/// Never delivered here: nothing tells a guest it is about to be killed.
pub fn error_termination_imminent() -> Handle {
    static ERROR: OnceLock<Handle> = OnceLock::new();
    *ERROR.get_or_init(|| error(c"Process termination imminent"))
}

/// `XPC_ERROR_KEY_DESCRIPTION`.
pub const KEY_DESCRIPTION: &CStr = c"XPCErrorDescription";

fn error(description: &CStr) -> Handle {
    let contents = Dictionary::new();
    contents.set_owned(KEY_DESCRIPTION, new(Value::String(description.to_owned())));
    new_global(Value::Error(contents))
}

/// A send right, deallocated when the last holder lets go.
struct SendRight(Name);

impl Drop for SendRight {
    fn drop(&mut self) {
        space::mach_port_deallocate(TASK_SELF, self.0);
    }
}

/// `xpc_endpoint_t`: a send right to a listener, which can travel in a message
/// where a connection cannot.
#[derive(Clone)]
pub struct Endpoint(Arc<SendRight>);

impl Endpoint {
    /// Take ownership of a send right that arrived in a message.
    pub fn adopt(name: Name) -> Endpoint {
        Endpoint(Arc::new(SendRight(name)))
    }

    pub fn port(&self) -> Name {
        self.0.0
    }
}

impl PartialEq for Endpoint {
    fn eq(&self, other: &Endpoint) -> bool {
        space::address_of(self.port()) == space::address_of(other.port())
    }
}

/// Who is on the other end, from the audit trailer of the message that
/// introduced them.
#[derive(Clone, Copy, Default)]
struct Peer {
    pid: i32,
    euid: u32,
    egid: u32,
    asid: i32,
}

impl Peer {
    fn from_audit(token: [u32; 8]) -> Peer {
        Peer {
            euid: token[1],
            egid: token[2],
            pid: token[5] as i32,
            asid: token[6] as i32,
        }
    }
}

enum Role {
    /// A mach service's listener, or an anonymous one: receives clients.
    Listener { service: Option<CString> },
    /// Connects to a mach service by name, or to an endpoint.
    Client { target: Target },
    /// The listener's end of one client.
    Peer,
}

enum Target {
    Service(CString),
    Endpoint(Endpoint),
}

/// A client's state toward its server.
enum Link {
    /// Not connected: before the first message, or after an interruption.
    Idle,
    /// Said hello and waiting for the welcome; messages wait in the outbox.
    Connecting,
    Connected,
}

/// A message waiting for the peer's port.
struct Outgoing {
    payload: Vec<u8>,
    ports: Vec<Name>,
    /// Keeps the endpoints the payload names alive until it is sent.
    _endpoints: Vec<Endpoint>,
}

/// Who waits for a reply.
enum Waiter {
    Sync(Arc<Slot>),
    Async { queue: u64, block: u64 },
}

#[derive(Default)]
struct Slot {
    reply: Mutex<Option<Handle>>,
    ready: Condvar,
}

impl Slot {
    fn fill(&self, reply: Handle) {
        *lock(&self.reply) = Some(reply);
        self.ready.notify_all();
    }

    fn wait(&self) -> Handle {
        let mut reply = lock(&self.reply);
        loop {
            if let Some(reply) = reply.take() {
                return reply;
            }
            reply = self.ready.wait(reply).unwrap_or_else(|e| e.into_inner());
        }
    }
}

enum Event {
    /// A message, an error or (for a listener) a new peer, owned.
    Object(Handle),
    /// `xpc_connection_send_barrier`'s block, owned.
    Barrier(u64),
}

struct State {
    handler: u64,
    context: u64,
    finalizer: u64,
    target_queue: u64,
    /// Starts at one: a connection is created suspended.
    suspended: u32,
    activated: bool,
    cancelled: bool,
    /// The final `XPC_ERROR_CONNECTION_INVALID` has been queued.
    invalidated: bool,
    events: VecDeque<Event>,
    draining: bool,
    /// This end's receive right.
    local: Name,
    /// A send right to the other end's port.
    remote: Name,
    link: Link,
    outbox: Vec<Outgoing>,
    waiters: HashMap<u64, Waiter>,
    next_sequence: u64,
    peer: Peer,
}

pub struct Connection {
    role: Role,
    state: Mutex<State>,
    /// The guest's object for this connection; not a reference of its own.
    handle: AtomicU64,
}

impl Connection {
    fn create(role: Role, target_queue: u64) -> Handle {
        if target_queue != 0 {
            crate::dispatch::object::retain(target_queue);
        }
        let connection = Arc::new(Connection {
            role,
            state: Mutex::new(State {
                handler: 0,
                context: 0,
                finalizer: 0,
                target_queue,
                suspended: 1,
                activated: false,
                cancelled: false,
                invalidated: false,
                events: VecDeque::new(),
                draining: false,
                local: MACH_PORT_NULL,
                remote: MACH_PORT_NULL,
                link: Link::Idle,
                outbox: Vec::new(),
                waiters: HashMap::new(),
                next_sequence: 1,
                peer: Peer::default(),
            }),
            handle: AtomicU64::new(0),
        });
        let handle = new(Value::Connection(Arc::clone(&connection)));
        connection.handle.store(handle, Ordering::Release);
        handle
    }

    fn handle(&self) -> Handle {
        self.handle.load(Ordering::Acquire)
    }

    fn name(&self) -> Option<&CStr> {
        match &self.role {
            Role::Listener { service } => service.as_deref(),
            Role::Client {
                target: Target::Service(name),
            } => Some(name),
            _ => None,
        }
    }

    pub fn describe(&self) -> String {
        let state = lock(&self.state);
        let name = self.name().map(|n| n.to_string_lossy().into_owned());
        format!(
            "name = {}, pid = {}, cancelled = {}",
            name.as_deref().unwrap_or("(anonymous)"),
            state.peer.pid,
            state.cancelled
        )
    }

    /// Queue `event` for the handler, running it when the connection may.
    fn deliver(&self, event: Event) {
        let mut state = lock(&self.state);
        state.events.push_back(event);
        self.schedule(&mut state);
    }

    fn schedule(&self, state: &mut State) {
        if state.suspended > 0 || state.draining || state.events.is_empty() {
            return;
        }
        state.draining = true;
        let target = target_of(state);
        let context = retain(self.handle());
        // SAFETY: `drain` takes the reference passed as its context.
        unsafe { queue::async_f(target, context, drain as *const () as usize as u64) };
    }

    /// Fail every reply still owed with `error`, and queue it for the handler.
    fn break_with(&self, error: Handle, state: &mut State) {
        for (_, waiter) in state.waiters.drain() {
            answer(waiter, retain(error));
        }
        if !state.invalidated {
            if error == error_invalid() {
                state.invalidated = true;
            }
            state.events.push_back(Event::Object(retain(error)));
        }
        self.schedule(state);
    }

    /// Give up both ports: the local right is destroyed and the remote one let
    /// go, so the other end sees no more senders. Answers the route's
    /// reference for the caller to release outside the lock.
    fn disconnect(state: &mut State) -> Option<Handle> {
        let route = match state.local {
            MACH_PORT_NULL => None,
            local => router().and_then(|r| r.remove(local)),
        };
        if state.local != MACH_PORT_NULL {
            space::destroy(state.local);
            state.local = MACH_PORT_NULL;
        }
        if state.remote != MACH_PORT_NULL {
            space::mach_port_deallocate(TASK_SELF, state.remote);
            state.remote = MACH_PORT_NULL;
        }
        state.link = Link::Idle;
        state.outbox.clear();
        route
    }

    /// The other end went away, or could not be reached.
    fn broken(&self) {
        let route = {
            let mut state = lock(&self.state);
            if state.cancelled || state.invalidated {
                return;
            }
            let route = Self::disconnect(&mut state);
            // A named service is looked up again by the next message, so its
            // client is only interrupted; anything else is gone for good.
            let error = match &self.role {
                Role::Client {
                    target: Target::Service(_),
                } => error_interrupted(),
                _ => error_invalid(),
            };
            self.break_with(error, &mut state);
            route
        };
        if let Some(route) = route {
            // SAFETY: the route's reference, taken out of the router.
            unsafe { release(route) };
        }
    }

    fn cancel(&self) {
        let route = {
            let mut state = lock(&self.state);
            if state.cancelled {
                return;
            }
            state.cancelled = true;
            let route = Self::disconnect(&mut state);
            for (_, waiter) in state.waiters.drain() {
                answer(waiter, retain(error_invalid()));
            }
            if !state.invalidated {
                state.invalidated = true;
                state
                    .events
                    .push_back(Event::Object(retain(error_invalid())));
            }
            self.schedule(&mut state);
            route
        };
        if let Some(route) = route {
            // SAFETY: as in [`Self::broken`].
            unsafe { release(route) };
        }
    }

    fn activate(&self) {
        let mut state = lock(&self.state);
        if state.activated || state.cancelled {
            return;
        }
        state.activated = true;
        if let Role::Listener {
            service: Some(service),
        } = &self.role
        {
            let mut port = MACH_PORT_NULL;
            // SAFETY: a NUL-terminated name and a writable port name.
            let rc = unsafe {
                bootstrap::bootstrap_check_in(
                    bootstrap::BOOTSTRAP_PORT,
                    service.as_ptr(),
                    &mut port,
                )
            };
            match rc == KERN_SUCCESS && listen_on(port, self.handle(), Watch::Ignore) {
                true => state.local = port,
                false => {
                    // Darwin's launchd refuses a service the job does not
                    // declare the same way: the listener is invalid at once.
                    state.invalidated = true;
                    state
                        .events
                        .push_back(Event::Object(retain(error_invalid())));
                }
            }
        }
    }

    /// Connect a client to its server if it is not already, under its lock.
    fn connect(&self, state: &mut State) -> Result<(), Handle> {
        let Role::Client { target } = &self.role else {
            return Ok(());
        };
        if state.cancelled || state.invalidated {
            return Err(error_invalid());
        }
        if !matches!(state.link, Link::Idle) {
            return Ok(());
        }
        let (server, looked_up) = match target {
            Target::Service(name) => {
                let mut port = MACH_PORT_NULL;
                // SAFETY: a NUL-terminated name and a writable port name.
                let rc = unsafe {
                    bootstrap::bootstrap_look_up(
                        bootstrap::BOOTSTRAP_PORT,
                        name.as_ptr(),
                        &mut port,
                    )
                };
                if rc != KERN_SUCCESS {
                    return Err(error_invalid());
                }
                (port, true)
            }
            Target::Endpoint(endpoint) => (endpoint.port(), false),
        };
        let local = allocate_receive();
        let hello = match local != MACH_PORT_NULL && listen_on(local, self.handle(), Watch::Peer) {
            true => send(
                server,
                msg::COPY_SEND,
                local,
                msg::MAKE_SEND,
                id::HELLO,
                &[],
                &[],
            ),
            false => KERN_FAILURE,
        };
        if looked_up {
            space::mach_port_deallocate(TASK_SELF, server);
        }
        state.local = local;
        if hello != KERN_SUCCESS {
            if let Some(route) = Self::disconnect(state) {
                // The route holds a reference this lock's caller also holds,
                // so this release never frees the connection.
                // SAFETY: the route's reference.
                unsafe { release(route) };
            }
            return Err(error_invalid());
        }
        state.link = Link::Connecting;
        Ok(())
    }

    /// Send `message`, numbered `sequence` when an answer is wanted.
    fn transmit(
        &self,
        message: Handle,
        waiter: Option<Waiter>,
    ) -> Result<(), (Handle, Option<Waiter>)> {
        // SAFETY: the caller's live message.
        let Some(dictionary) = (unsafe { collection::dictionary(message) }) else {
            return Err((error_invalid(), waiter));
        };
        let reply_to = dictionary.origin().and_then(|origin| origin.reply);
        let mut state = lock(&self.state);
        // Nothing would ever answer a waiter left on a connection that is
        // already over.
        if matches!(self.role, Role::Listener { .. }) || state.cancelled || state.invalidated {
            return Err((error_invalid(), waiter));
        }
        if let Err(error) = self.connect(&mut state) {
            if !state.invalidated {
                // The handler hears of it too, as Darwin's does.
                self.break_with(error, &mut state);
            }
            return Err((error, waiter));
        }
        let (message_kind, sequence, flags) = match (reply_to, &waiter) {
            (Some(sequence), _) => (kind::REPLY, sequence, 0),
            (None, Some(_)) => {
                let sequence = state.next_sequence;
                state.next_sequence += 1;
                (kind::REQUEST, sequence, WANTS_REPLY)
            }
            (None, None) => (kind::REQUEST, 0, 0),
        };
        let mut payload = Vec::new();
        payload.extend_from_slice(&message_kind.to_le_bytes());
        payload.extend_from_slice(&flags.to_le_bytes());
        payload.extend_from_slice(&sequence.to_le_bytes());
        let mut ports = Vec::new();
        // SAFETY: the dictionary checked above.
        let value = unsafe { object::value(message) }.expect("checked above");
        if codec::encode(value, &mut payload, &mut ports).is_err() {
            announce_unencodable();
            return Err((error_invalid(), waiter));
        }
        let endpoints = endpoints_of(&ports);
        if let Some(waiter) = waiter {
            state.waiters.insert(sequence, waiter);
        }
        match state.link {
            Link::Connecting => {
                state.outbox.push(Outgoing {
                    payload,
                    ports,
                    _endpoints: endpoints,
                });
                return Ok(());
            }
            Link::Idle | Link::Connected => {}
        }
        let remote = state.remote;
        drop(state);
        let descriptors: Vec<(Name, u32)> = ports.iter().map(|p| (*p, msg::COPY_SEND)).collect();
        let sent = send(
            remote,
            msg::COPY_SEND,
            MACH_PORT_NULL,
            0,
            id::MESSAGE,
            &descriptors,
            &payload,
        );
        drop(endpoints);
        if sent != KERN_SUCCESS {
            if sent == MACH_SEND_TOO_LARGE {
                announce_too_large();
            }
            // The waiter, if any, is failed with everything else owed.
            self.broken();
        }
        Ok(())
    }

    /// A message arrived on this connection's port.
    fn receive(&self, incoming: Incoming) {
        match (incoming.id, &self.role) {
            (NOTIFY_NO_SENDERS, Role::Listener { .. }) => {}
            (NOTIFY_NO_SENDERS, _) => self.broken(),
            (id::HELLO, Role::Listener { .. }) => self.accept(incoming),
            (id::WELCOME, Role::Client { .. }) => self.welcome(incoming),
            (id::MESSAGE, _) => self.message(incoming),
            _ => drop(incoming),
        }
    }

    /// A new client: make its peer connection and hand it to the handler.
    fn accept(&self, mut incoming: Incoming) {
        let client = std::mem::replace(&mut incoming.reply, MACH_PORT_NULL);
        if client == MACH_PORT_NULL {
            return;
        }
        let peer = Connection::create(Role::Peer, 0);
        // SAFETY: just created.
        let Some(Value::Connection(connection)) = (unsafe { object::value(peer) }) else {
            return;
        };
        let local = allocate_receive();
        let ready = local != MACH_PORT_NULL && listen_on(local, peer, Watch::Peer);
        {
            let mut state = lock(&connection.state);
            state.local = local;
            state.remote = client;
            state.link = Link::Connected;
            state.peer = Peer::from_audit(incoming.audit);
        }
        if !ready
            || send(
                client,
                msg::COPY_SEND,
                local,
                msg::MAKE_SEND,
                id::WELCOME,
                &[],
                &[],
            ) != KERN_SUCCESS
        {
            connection.broken();
        }
        // The handler's reference; the route keeps the one it took.
        self.deliver(Event::Object(peer));
    }

    /// The server answered: its port, for the messages waiting on it.
    fn welcome(&self, mut incoming: Incoming) {
        let server = std::mem::replace(&mut incoming.reply, MACH_PORT_NULL);
        let outbox = {
            let mut state = lock(&self.state);
            if !matches!(state.link, Link::Connecting) || server == MACH_PORT_NULL {
                drop(state);
                if server != MACH_PORT_NULL {
                    space::mach_port_deallocate(TASK_SELF, server);
                }
                return;
            }
            state.remote = server;
            state.link = Link::Connected;
            state.peer = Peer::from_audit(incoming.audit);
            std::mem::take(&mut state.outbox)
        };
        for outgoing in outbox {
            let descriptors: Vec<(Name, u32)> = outgoing
                .ports
                .iter()
                .map(|p| (*p, msg::COPY_SEND))
                .collect();
            let sent = send(
                server,
                msg::COPY_SEND,
                MACH_PORT_NULL,
                0,
                id::MESSAGE,
                &descriptors,
                &outgoing.payload,
            );
            if sent != KERN_SUCCESS {
                self.broken();
                return;
            }
        }
    }

    fn message(&self, mut incoming: Incoming) {
        let mut reader = Reader::new(&incoming.payload, &mut incoming.ports);
        let header =
            (|| Ok::<_, codec::Malformed>((reader.u32()?, reader.u32()?, reader.u64()?)))();
        let Ok((message_kind, flags, sequence)) = header else {
            return;
        };
        let origin = Origin {
            connection: retain(self.handle()),
            reply: (message_kind == kind::REQUEST && flags & WANTS_REPLY != 0).then_some(sequence),
        };
        let Ok(message) = reader.dictionary_into(Dictionary::with_origin(origin)) else {
            return;
        };
        if message_kind == kind::REPLY {
            let waiter = lock(&self.state).waiters.remove(&sequence);
            match waiter {
                Some(waiter) => answer(waiter, message),
                // SAFETY: the decoded message, which nobody waits for.
                None => unsafe { release(message) },
            }
            return;
        }
        self.deliver(Event::Object(message));
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        if state.local != MACH_PORT_NULL {
            space::destroy(state.local);
        }
        if state.remote != MACH_PORT_NULL {
            space::mach_port_deallocate(TASK_SELF, state.remote);
        }
        for event in state.events.drain(..) {
            release_event(event);
        }
        for (_, waiter) in state.waiters.drain() {
            answer(waiter, retain(error_invalid()));
        }
        block::release(state.handler);
        let target = target_of(state);
        if state.finalizer != 0 {
            // SAFETY: a `xpc_finalizer_t`, run on the target queue as Darwin does.
            unsafe { queue::async_f(target, state.context, state.finalizer) };
        }
        if state.target_queue != 0 {
            // SAFETY: the reference [`Connection::create`] took.
            unsafe { crate::dispatch::object::release(state.target_queue) };
        }
    }
}

fn target_of(state: &State) -> u64 {
    match state.target_queue {
        // SAFETY: no arguments that matter; the default-priority global queue.
        0 => unsafe { queue::get_global_queue(0, 0) },
        queue => queue,
    }
}

fn release_event(event: Event) {
    match event {
        // SAFETY: the event's own reference.
        Event::Object(handle) => unsafe { release(handle) },
        Event::Barrier(barrier) => block::release(barrier),
    }
}

/// Hand `reply` to whoever waits for it.
fn answer(waiter: Waiter, reply: Handle) {
    match waiter {
        Waiter::Sync(slot) => slot.fill(reply),
        Waiter::Async { queue: q, block: b } => {
            let context = Box::into_raw(Box::new((b, reply))) as u64;
            // SAFETY: `run_reply` takes the box passed as its context.
            unsafe { queue::async_f(q, context, run_reply as *const () as usize as u64) };
        }
    }
}

unsafe extern "C" fn run_reply(context: *mut c_void) {
    // SAFETY: the box [`answer`] made.
    let (handler, reply) = *unsafe { Box::from_raw(context as *mut (u64, Handle)) };
    // SAFETY: the reply handler's block, called with an object as it expects.
    unsafe { block::invoke_with(handler, reply) };
    block::release(handler);
    // SAFETY: the reply's reference; the handler retains it if it keeps it.
    unsafe { release(reply) };
}

/// Run a connection's events, one at a time, until it has none or is suspended.
unsafe extern "C" fn drain(context: *mut c_void) {
    let handle = context as Handle;
    // SAFETY: the reference [`Connection::schedule`] passed.
    if let Some(Value::Connection(connection)) = unsafe { object::value(handle) } {
        loop {
            let (event, handler) = {
                let mut state = lock(&connection.state);
                let next = match state.suspended {
                    0 => state.events.pop_front(),
                    _ => None,
                };
                let Some(event) = next else {
                    state.draining = false;
                    break;
                };
                (event, block::copy(state.handler).unwrap_or(0))
            };
            let last = matches!(event, Event::Object(e) if e == error_invalid());
            match event {
                Event::Object(event) => {
                    // SAFETY: the connection's handler, which takes an object.
                    unsafe { block::invoke_with(handler, event) };
                    // SAFETY: the event's reference.
                    unsafe { release(event) };
                }
                Event::Barrier(barrier) => {
                    // SAFETY: a `dispatch_block_t`.
                    unsafe { block::invoke(barrier) };
                    block::release(barrier);
                }
            }
            block::release(handler);
            if last {
                // Nothing follows the invalidation; Darwin drops the handler
                // so the captures it holds are freed.
                let old = std::mem::take(&mut lock(&connection.state).handler);
                block::release(old);
            }
        }
    }
    // SAFETY: as above.
    unsafe { release(handle) };
}

fn endpoints_of(ports: &[Name]) -> Vec<Endpoint> {
    // Each port is an endpoint's right; another reference to it keeps the
    // name from being deallocated while a message naming it waits.
    ports
        .iter()
        .map(|port| {
            space::mach_port_mod_refs(TASK_SELF, *port, crate::mach::space::RIGHT_SEND, 1);
            Endpoint::adopt(*port)
        })
        .collect()
}

fn announce_unencodable() {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, Ordering::Relaxed) {
        eprintln!(
            "rosemary: xpc_connection_send_message was given a message holding a connection or an \
             error, which cannot be sent; Darwin aborts the sender, this drops the message"
        );
    }
}

fn announce_too_large() {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, Ordering::Relaxed) {
        eprintln!(
            "rosemary: an XPC message was larger than one Mach message carries here, and was \
             refused - Darwin would have moved it out of line"
        );
    }
}

fn allocate_receive() -> Name {
    let mut name = MACH_PORT_NULL;
    // SAFETY: a writable name.
    let rc = unsafe { space::mach_port_allocate(TASK_SELF, RIGHT_RECEIVE, &mut name) };
    match rc {
        KERN_SUCCESS => name,
        _ => MACH_PORT_NULL,
    }
}

/// Send a message: header, optional port descriptors, then `payload`.
fn send(
    remote: Name,
    remote_disposition: u32,
    local: Name,
    local_disposition: u32,
    msgh_id: i32,
    ports: &[(Name, u32)],
    payload: &[u8],
) -> i32 {
    let complex = !ports.is_empty();
    let body = match complex {
        true => BODY + ports.len() * DESCRIPTOR,
        false => 0,
    };
    let size = (HEADER + body + payload.len()).next_multiple_of(4);
    let mut buffer = vec![0u8; size];
    let mut bits = remote_disposition | (local_disposition << BITS_LOCAL_SHIFT);
    if complex {
        bits |= BITS_COMPLEX;
    }
    let put = |buffer: &mut [u8], at: usize, value: u32| {
        buffer[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    put(&mut buffer, 0, bits);
    put(&mut buffer, 4, size as u32);
    put(&mut buffer, 8, remote);
    put(&mut buffer, 12, local);
    put(&mut buffer, 20, msgh_id as u32);
    if complex {
        put(&mut buffer, HEADER, ports.len() as u32);
        for (i, (name, disposition)) in ports.iter().enumerate() {
            let at = HEADER + BODY + i * DESCRIPTOR;
            put(&mut buffer, at, *name);
            buffer[at + DESCRIPTOR_DISPOSITION_AT] = *disposition as u8;
            buffer[at + DESCRIPTOR_TYPE_AT] = PORT_DESCRIPTOR;
        }
    }
    buffer[HEADER + body..HEADER + body + payload.len()].copy_from_slice(payload);
    // SAFETY: a message of `size` bytes, built above.
    unsafe {
        msg::mach_msg(
            buffer.as_mut_ptr().cast(),
            SEND_MSG,
            size as u32,
            0,
            MACH_PORT_NULL,
            0,
            MACH_PORT_NULL,
        )
    }
}

/// A received message, taken apart.
struct Incoming {
    id: i32,
    /// The reply port's send right, if one came.
    reply: Name,
    /// The send rights in its port descriptors, until the payload claims them.
    ports: Vec<Option<Name>>,
    payload: Vec<u8>,
    audit: [u32; 8],
}

impl Drop for Incoming {
    fn drop(&mut self) {
        let unclaimed = self.ports.iter().flatten().copied();
        for name in unclaimed.chain((self.reply != MACH_PORT_NULL).then_some(self.reply)) {
            space::mach_port_deallocate(TASK_SELF, name);
        }
    }
}

/// The receiving thread's port set and the connections it serves.
struct Router {
    set: Name,
    wake: Name,
    /// Each port's connection, retained while the port is routed.
    routes: Mutex<HashMap<Name, Handle>>,
}

impl Router {
    fn add(&self, port: Name, connection: Handle) -> bool {
        if space::mach_port_insert_member(TASK_SELF, port, self.set) != KERN_SUCCESS {
            return false;
        }
        lock(&self.routes).insert(port, retain(connection));
        self.wake();
        true
    }

    fn remove(&self, port: Name) -> Option<Handle> {
        space::mach_port_move_member(TASK_SELF, port, MACH_PORT_NULL);
        let removed = lock(&self.routes).remove(&port);
        self.wake();
        removed
    }

    /// End the thread's current wait, so it receives on the set as it is now.
    fn wake(&self) {
        send(
            self.wake,
            msg::MAKE_SEND,
            MACH_PORT_NULL,
            0,
            id::WAKE,
            &[],
            &[],
        );
    }

    fn run(&self) {
        let mut buffer = vec![0u8; crate::mach::wire::MAX_MESSAGE + HEADER + AUDIT_TRAILER];
        loop {
            // SAFETY: a buffer of the length given.
            let rc = unsafe {
                msg::mach_msg(
                    buffer.as_mut_ptr().cast(),
                    RCV_MSG | RCV_LARGE | RCV_TRAILER_AUDIT,
                    0,
                    buffer.len() as u32,
                    self.set,
                    0,
                    MACH_PORT_NULL,
                )
            };
            if rc != KERN_SUCCESS {
                continue;
            }
            let Some((arrived_on, incoming)) = parse(&buffer) else {
                continue;
            };
            if arrived_on == self.wake {
                continue;
            }
            let connection = lock(&self.routes).get(&arrived_on).map(|h| retain(*h));
            let Some(connection) = connection else {
                continue;
            };
            // SAFETY: the reference just taken.
            if let Some(Value::Connection(c)) = unsafe { object::value(connection) } {
                c.receive(incoming);
            }
            // SAFETY: as above.
            unsafe { release(connection) };
        }
    }
}

/// The port a received message arrived on, and the message.
fn parse(buffer: &[u8]) -> Option<(Name, Incoming)> {
    let word = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(buffer.get(at..at + 4)?.try_into().ok()?))
    };
    let bits = word(0)?;
    let size = word(4)? as usize;
    let reply = word(8)?;
    let arrived_on = word(12)?;
    let msgh_id = word(20)? as i32;
    let mut at = HEADER;
    let mut ports = Vec::new();
    if bits & BITS_COMPLEX != 0 {
        let count = word(HEADER)? as usize;
        at += BODY;
        for i in 0..count {
            let descriptor = HEADER + BODY + i * DESCRIPTOR;
            let is_port = *buffer.get(descriptor + DESCRIPTOR_TYPE_AT)? == PORT_DESCRIPTOR;
            ports.push(
                is_port
                    .then(|| word(descriptor))
                    .flatten()
                    .filter(|n| *n != MACH_PORT_NULL),
            );
            at += DESCRIPTOR;
        }
    }
    let payload = buffer.get(at..size)?.to_vec();
    let mut audit = [0u32; 8];
    for (i, slot) in audit.iter_mut().enumerate() {
        *slot = word(size + AUDIT_AT + i * 4)?;
    }
    Some((
        arrived_on,
        Incoming {
            id: msgh_id,
            reply,
            ports,
            payload,
            audit,
        },
    ))
}

fn router() -> Option<&'static Router> {
    static ROUTER: OnceLock<Option<&'static Router>> = OnceLock::new();
    *ROUTER.get_or_init(|| {
        let mut set = MACH_PORT_NULL;
        // SAFETY: a writable name.
        if unsafe { space::mach_port_allocate(TASK_SELF, RIGHT_PORT_SET, &mut set) } != KERN_SUCCESS
        {
            return None;
        }
        let wake = allocate_receive();
        if wake == MACH_PORT_NULL
            || space::mach_port_insert_member(TASK_SELF, wake, set) != KERN_SUCCESS
        {
            return None;
        }
        let router: &'static Router = Box::leak(Box::new(Router {
            set,
            wake,
            routes: Mutex::new(HashMap::new()),
        }));
        std::thread::Builder::new()
            .name("xpc".into())
            .spawn(move || router.run())
            .ok()?;
        Some(router)
    })
}

/// Route `port` to `connection`.
///
/// A connection end is told when its peer's send right is gone, which is how
/// it learns the peer is. A listener is not: clients come and go, and one
/// that looked the service up and let go is no reason for it to stop.
fn listen_on(port: Name, connection: Handle, peer: Watch) -> bool {
    let Some(router) = router() else {
        return false;
    };
    if matches!(peer, Watch::Ignore) {
        return router.add(port, connection);
    }
    let mut previous = MACH_PORT_NULL;
    // SAFETY: a writable name; the notification goes to the port itself.
    unsafe {
        space::mach_port_request_notification(
            TASK_SELF,
            port,
            NOTIFY_NO_SENDERS,
            0,
            port,
            msg::MAKE_SEND_ONCE,
            &mut previous,
        )
    };
    router.add(port, connection)
}

/// Whether a port's lack of senders means its peer is gone.
enum Watch {
    Peer,
    Ignore,
}

/// # Safety
///
/// `handle` is null or a live object of this crate's.
unsafe fn connection<'a>(handle: Handle) -> Option<&'a Arc<Connection>> {
    match unsafe { object::value(handle) }? {
        Value::Connection(connection) => Some(connection),
        _ => None,
    }
}

/// `xpc_connection_t xpc_connection_create(const char *name, dispatch_queue_t)`:
/// with a name, a client of that service; without, an anonymous listener whose
/// endpoint can be sent to a client.
///
/// # Safety
///
/// `name` is null or a NUL-terminated string.
pub unsafe extern "C" fn xpc_connection_create(name: *const c_char, target: u64) -> Handle {
    match unsafe { object::c_string(name) } {
        Some(name) => Connection::create(
            Role::Client {
                target: Target::Service(name.to_owned()),
            },
            target,
        ),
        None => {
            let handle = Connection::create(Role::Listener { service: None }, target);
            let port = allocate_receive();
            // SAFETY: just created.
            if let Some(connection) = unsafe { connection(handle) }
                && port != MACH_PORT_NULL
                && listen_on(port, handle, Watch::Ignore)
            {
                lock(&connection.state).local = port;
            }
            handle
        }
    }
}

/// `xpc_connection_t xpc_connection_create_mach_service(const char *name,
/// dispatch_queue_t, uint64_t flags)`.
///
/// # Safety
///
/// `name` is a NUL-terminated string.
pub unsafe extern "C" fn xpc_connection_create_mach_service(
    name: *const c_char,
    target: u64,
    flags: u64,
) -> Handle {
    let Some(name) = (unsafe { object::c_string(name) }) else {
        return 0;
    };
    // `XPC_CONNECTION_MACH_SERVICE_PRIVILEGED` asks that the service be the
    // system domain's; there is one domain here, so it changes nothing.
    let role = match flags & MACH_SERVICE_LISTENER != 0 {
        true => Role::Listener {
            service: Some(name.to_owned()),
        },
        false => Role::Client {
            target: Target::Service(name.to_owned()),
        },
    };
    Connection::create(role, target)
}

/// `xpc_connection_t xpc_connection_create_from_endpoint(xpc_endpoint_t)`.
pub unsafe extern "C" fn xpc_connection_create_from_endpoint(endpoint: Handle) -> Handle {
    let Some(Value::Endpoint(endpoint)) = (unsafe { object::value(endpoint) }) else {
        return 0;
    };
    Connection::create(
        Role::Client {
            target: Target::Endpoint(endpoint.clone()),
        },
        0,
    )
}

/// `xpc_endpoint_t xpc_endpoint_create(xpc_connection_t)`: only a listener has one.
pub unsafe extern "C" fn xpc_endpoint_create(handle: Handle) -> Handle {
    let Some(connection) = (unsafe { connection(handle) }) else {
        return 0;
    };
    if !matches!(connection.role, Role::Listener { .. }) {
        return 0;
    }
    let local = lock(&connection.state).local;
    let Some(address) = space::address_of(local) else {
        return 0;
    };
    new(Value::Endpoint(Endpoint::adopt(space::insert_send(
        address,
    ))))
}

/// `void xpc_connection_set_event_handler(xpc_connection_t, xpc_handler_t)`.
pub unsafe extern "C" fn xpc_connection_set_event_handler(handle: Handle, handler: u64) {
    let Some(connection) = (unsafe { connection(handle) }) else {
        return;
    };
    let copied = block::copy(handler).unwrap_or(0);
    let old = std::mem::replace(&mut lock(&connection.state).handler, copied);
    block::release(old);
}

/// `void xpc_connection_set_target_queue(xpc_connection_t, dispatch_queue_t)`.
pub unsafe extern "C" fn xpc_connection_set_target_queue(handle: Handle, target: u64) {
    let Some(connection) = (unsafe { connection(handle) }) else {
        return;
    };
    if target != 0 {
        crate::dispatch::object::retain(target);
    }
    let old = std::mem::replace(&mut lock(&connection.state).target_queue, target);
    if old != 0 {
        // SAFETY: the reference taken when it was set.
        unsafe { crate::dispatch::object::release(old) };
    }
}

/// `void xpc_connection_resume(xpc_connection_t)`: the first resume also
/// activates it.
pub unsafe extern "C" fn xpc_connection_resume(handle: Handle) {
    let Some(connection) = (unsafe { connection(handle) }) else {
        return;
    };
    connection.activate();
    let mut state = lock(&connection.state);
    // Darwin aborts on a resume without a suspend; this ignores it.
    if state.suspended > 0 {
        state.suspended -= 1;
    }
    connection.schedule(&mut state);
}

/// `void xpc_connection_activate(xpc_connection_t)`.
pub unsafe extern "C" fn xpc_connection_activate(handle: Handle) {
    let Some(connection) = (unsafe { connection(handle) }) else {
        return;
    };
    let first = !lock(&connection.state).activated;
    if first {
        // SAFETY: a live connection.
        unsafe { xpc_connection_resume(handle) };
    }
}

pub unsafe extern "C" fn xpc_connection_suspend(handle: Handle) {
    if let Some(connection) = unsafe { connection(handle) } {
        lock(&connection.state).suspended += 1;
    }
}

pub unsafe extern "C" fn xpc_connection_cancel(handle: Handle) {
    if let Some(connection) = unsafe { connection(handle) } {
        connection.cancel();
    }
}

/// `void xpc_connection_send_message(xpc_connection_t, xpc_object_t)`.
pub unsafe extern "C" fn xpc_connection_send_message(handle: Handle, message: Handle) {
    if let Some(connection) = unsafe { connection(handle) } {
        let _ = connection.transmit(message, None);
    }
}

/// `void xpc_connection_send_message_with_reply(xpc_connection_t, xpc_object_t,
/// dispatch_queue_t, xpc_handler_t)`.
pub unsafe extern "C" fn xpc_connection_send_message_with_reply(
    handle: Handle,
    message: Handle,
    reply_queue: u64,
    handler: u64,
) {
    let Some(connection) = (unsafe { connection(handle) }) else {
        return;
    };
    let Some(handler) = block::copy(handler) else {
        return;
    };
    let reply_queue = match reply_queue {
        0 => target_of(&lock(&connection.state)),
        queue => queue,
    };
    let waiter = Waiter::Async {
        queue: reply_queue,
        block: handler,
    };
    if let Err((error, Some(waiter))) = connection.transmit(message, Some(waiter)) {
        answer(waiter, retain(error));
    }
}

/// `xpc_object_t xpc_connection_send_message_with_reply_sync(xpc_connection_t,
/// xpc_object_t)`: the reply, or the error that ended the wait, owned.
pub unsafe extern "C" fn xpc_connection_send_message_with_reply_sync(
    handle: Handle,
    message: Handle,
) -> Handle {
    let Some(connection) = (unsafe { connection(handle) }) else {
        return retain(error_invalid());
    };
    let slot = Arc::new(Slot::default());
    match connection.transmit(message, Some(Waiter::Sync(Arc::clone(&slot)))) {
        Ok(()) => slot.wait(),
        Err((error, _)) => retain(error),
    }
}

/// `void xpc_connection_send_barrier(xpc_connection_t, dispatch_block_t)`.
///
/// Every message sent before it has been handed to the transport by the time
/// it returns, so the barrier only has to follow the events already queued.
pub unsafe extern "C" fn xpc_connection_send_barrier(handle: Handle, barrier: u64) {
    if let (Some(connection), Some(barrier)) = (unsafe { connection(handle) }, block::copy(barrier))
    {
        connection.deliver(Event::Barrier(barrier));
    }
}

pub unsafe extern "C" fn xpc_connection_get_name(handle: Handle) -> *const c_char {
    unsafe { connection(handle) }
        .and_then(|c| c.name())
        .map_or(std::ptr::null(), CStr::as_ptr)
}

fn peer_of(handle: Handle) -> Peer {
    // SAFETY: the guest's live connection.
    unsafe { connection(handle) }.map_or(Peer::default(), |c| lock(&c.state).peer)
}

pub unsafe extern "C" fn xpc_connection_get_pid(handle: Handle) -> i32 {
    peer_of(handle).pid
}

pub unsafe extern "C" fn xpc_connection_get_euid(handle: Handle) -> u32 {
    peer_of(handle).euid
}

pub unsafe extern "C" fn xpc_connection_get_egid(handle: Handle) -> u32 {
    peer_of(handle).egid
}

pub unsafe extern "C" fn xpc_connection_get_asid(handle: Handle) -> i32 {
    peer_of(handle).asid
}

pub unsafe extern "C" fn xpc_connection_set_context(handle: Handle, context: *mut c_void) {
    if let Some(connection) = unsafe { connection(handle) } {
        lock(&connection.state).context = context as u64;
    }
}

pub unsafe extern "C" fn xpc_connection_get_context(handle: Handle) -> *mut c_void {
    unsafe { connection(handle) }.map_or(std::ptr::null_mut(), |c| {
        lock(&c.state).context as *mut c_void
    })
}

pub unsafe extern "C" fn xpc_connection_set_finalizer_f(handle: Handle, finalizer: u64) {
    if let Some(connection) = unsafe { connection(handle) } {
        lock(&connection.state).finalizer = finalizer;
    }
}

/// `xpc_object_t xpc_connection_copy_entitlement_value(xpc_connection_t,
/// const char *)`: no process here is signed with entitlements, so none has
/// any value for one.
pub unsafe extern "C" fn xpc_connection_copy_entitlement_value(
    _handle: Handle,
    _entitlement: *const c_char,
) -> Handle {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xpc::collection::*;
    use std::sync::atomic::AtomicU32;

    /// A block literal whose `invoke` is a host function, as a guest's would
    /// be once its pointers are thunks. Global, so copying it is free.
    #[repr(C)]
    struct Literal {
        isa: u64,
        flags: u32,
        reserved: u32,
        invoke: u64,
        descriptor: u64,
    }

    /// `BLOCK_IS_GLOBAL`: `_Block_copy` answers the block itself.
    const BLOCK_IS_GLOBAL: u32 = 1 << 28;

    fn literal(invoke: u64) -> &'static Literal {
        Box::leak(Box::new(Literal {
            isa: 0,
            flags: BLOCK_IS_GLOBAL,
            reserved: 0,
            invoke,
            descriptor: 0,
        }))
    }

    fn block_of(literal: &'static Literal) -> u64 {
        literal as *const Literal as u64
    }

    /// The block runtime the tests run without: copies of global blocks only.
    fn install_block_runtime() {
        extern "C" fn copy(block: u64) -> u64 {
            block
        }
        extern "C" fn release(_block: u64) {}
        block::set_runtime(copy as *const () as u64, release as *const () as u64);
    }

    fn unique_service(what: &str) -> CString {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        CString::new(format!(
            "org.rosemary.test.xpc.{what}.{}.{n}",
            std::process::id()
        ))
        .unwrap()
    }

    /// Echoes each request's `value` back with `echo` set, or hangs up on a
    /// request carrying `hangup`.
    extern "C" fn echo_peer(_block: u64, event: Handle) {
        // SAFETY: an event the connection delivered.
        unsafe {
            if object::xpc_get_type(event) != object::Type::Dictionary.token() {
                return;
            }
            let peer = xpc_dictionary_get_remote_connection(event);
            if xpc_dictionary_get_bool(event, c"hangup".as_ptr()) {
                xpc_connection_cancel(peer);
                return;
            }
            let reply = xpc_dictionary_create_reply(event);
            assert_ne!(
                reply, 0,
                "a request that waits for an answer can be answered"
            );
            let value = xpc_dictionary_get_int64(event, c"value".as_ptr());
            xpc_dictionary_set_int64(reply, c"echo".as_ptr(), value);
            xpc_dictionary_set_int64(
                reply,
                c"client".as_ptr(),
                xpc_connection_get_pid(peer) as i64,
            );
            xpc_connection_send_message(peer, reply);
            release(reply);
        }
    }

    extern "C" fn accept_peer(_block: u64, event: Handle) {
        // SAFETY: as above.
        unsafe {
            if object::xpc_get_type(event) != object::Type::Connection.token() {
                return;
            }
            static HANDLER: OnceLock<u64> = OnceLock::new();
            let handler = *HANDLER.get_or_init(|| block_of(literal(echo_peer as *const () as u64)));
            xpc_connection_set_event_handler(event, handler);
            xpc_connection_resume(event);
        }
    }

    extern "C" fn ignore(_block: u64, _event: Handle) {}

    fn listen(service: &CStr) -> Handle {
        install_block_runtime();
        // SAFETY: a NUL-terminated name.
        unsafe {
            let listener =
                xpc_connection_create_mach_service(service.as_ptr(), 0, MACH_SERVICE_LISTENER);
            xpc_connection_set_event_handler(
                listener,
                block_of(literal(accept_peer as *const () as u64)),
            );
            xpc_connection_resume(listener);
            listener
        }
    }

    fn request(client: Handle, value: i64) -> Handle {
        // SAFETY: a live client connection.
        unsafe {
            let message = xpc_dictionary_create_empty();
            xpc_dictionary_set_int64(message, c"value".as_ptr(), value);
            let reply = xpc_connection_send_message_with_reply_sync(client, message);
            release(message);
            reply
        }
    }

    #[test]
    fn a_request_to_a_mach_service_is_answered() {
        let service = unique_service("echo");
        let listener = listen(&service);
        // SAFETY: live connections of this module's.
        unsafe {
            let client = xpc_connection_create_mach_service(service.as_ptr(), 0, 0);
            xpc_connection_set_event_handler(client, block_of(literal(ignore as *const () as u64)));
            xpc_connection_resume(client);
            for value in [7, -3, 1 << 40] {
                let reply = request(client, value);
                assert_eq!(
                    object::xpc_get_type(reply),
                    object::Type::Dictionary.token()
                );
                assert_eq!(xpc_dictionary_get_int64(reply, c"echo".as_ptr()), value);
                // Only Linux's kernel says who sent a datagram.
                if cfg!(target_os = "linux") {
                    let me = crate::process::getpid();
                    assert_eq!(
                        xpc_dictionary_get_int64(reply, c"client".as_ptr()),
                        me as i64,
                        "the server's view"
                    );
                    assert_eq!(xpc_connection_get_pid(client), me, "the client's view");
                }
                release(reply);
            }
            xpc_connection_cancel(client);
            release(client);
            xpc_connection_cancel(listener);
            release(listener);
        }
    }

    #[test]
    fn a_service_nobody_offers_is_invalid() {
        install_block_runtime();
        let service = unique_service("absent");
        // SAFETY: as above.
        unsafe {
            let client = xpc_connection_create_mach_service(service.as_ptr(), 0, 0);
            xpc_connection_set_event_handler(client, block_of(literal(ignore as *const () as u64)));
            xpc_connection_resume(client);
            let reply = request(client, 1);
            assert_eq!(reply, error_invalid());
            assert_eq!(
                CStr::from_ptr(xpc_dictionary_get_string(reply, KEY_DESCRIPTION.as_ptr())),
                c"Connection invalid"
            );
            xpc_connection_cancel(client);
            release(client);
        }
    }

    #[test]
    fn a_server_hanging_up_interrupts_its_client_and_the_next_message_reconnects() {
        let service = unique_service("hangup");
        let listener = listen(&service);
        // SAFETY: as above.
        unsafe {
            let client = xpc_connection_create_mach_service(service.as_ptr(), 0, 0);
            xpc_connection_set_event_handler(client, block_of(literal(ignore as *const () as u64)));
            xpc_connection_resume(client);

            let hangup = xpc_dictionary_create_empty();
            xpc_dictionary_set_bool(hangup, c"hangup".as_ptr(), true);
            let reply = xpc_connection_send_message_with_reply_sync(client, hangup);
            release(hangup);
            assert_eq!(
                reply,
                error_interrupted(),
                "the wait ends when the peer goes"
            );

            // A mach service's client looks the service up again.
            let reply = request(client, 5);
            assert_eq!(xpc_dictionary_get_int64(reply, c"echo".as_ptr()), 5);
            release(reply);
            xpc_connection_cancel(client);
            release(client);
            xpc_connection_cancel(listener);
            release(listener);
        }
    }

    /// A cancelled connection's later requests answer at once rather than
    /// wait for a peer that can no longer reply.
    #[test]
    fn a_cancelled_connection_answers_its_requests_with_invalid() {
        let service = unique_service("cancelled");
        let listener = listen(&service);
        // SAFETY: as above.
        unsafe {
            let client = xpc_connection_create_mach_service(service.as_ptr(), 0, 0);
            xpc_connection_set_event_handler(client, block_of(literal(ignore as *const () as u64)));
            xpc_connection_resume(client);
            let reply = request(client, 3);
            assert_eq!(xpc_dictionary_get_int64(reply, c"echo".as_ptr()), 3);
            release(reply);
            xpc_connection_cancel(client);
            assert_eq!(request(client, 4), error_invalid());
            release(client);
            xpc_connection_cancel(listener);
            release(listener);
        }
    }

    #[test]
    fn an_endpoint_connects_to_an_anonymous_listener() {
        install_block_runtime();
        // SAFETY: as above.
        unsafe {
            let listener = xpc_connection_create(std::ptr::null(), 0);
            xpc_connection_set_event_handler(
                listener,
                block_of(literal(accept_peer as *const () as u64)),
            );
            xpc_connection_resume(listener);
            let endpoint = xpc_endpoint_create(listener);
            assert_eq!(
                object::xpc_get_type(endpoint),
                object::Type::Endpoint.token()
            );
            let client = xpc_connection_create_from_endpoint(endpoint);
            xpc_connection_set_event_handler(client, block_of(literal(ignore as *const () as u64)));
            xpc_connection_resume(client);
            let reply = request(client, 42);
            assert_eq!(xpc_dictionary_get_int64(reply, c"echo".as_ptr()), 42);
            release(reply);
            xpc_connection_cancel(client);
            release(client);
            release(endpoint);
            xpc_connection_cancel(listener);
            release(listener);
        }
    }
}
