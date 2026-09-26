//! `<xpc/xpc.h>` and `<xpc/connection.h>`, as far as they can be answered truthfully.
//!
//! # What this is for
//!
//! PLAN.md §5 step 3 item 4 and §8 list `xpc_*` as "entirely absent" and name
//! crashpad as the consumer. The binary disagrees, and the binary is the
//! authority (§10.4), so this module is written against what it says:
//!
//! * **The guest imports no `xpc_*` symbol at all.** Every one of the 4029
//!   entries of the Framework's indirect symbol table resolves (2254 distinct
//!   names, of which 1579 have a `__TEXT,__stubs` slot) and not one begins
//!   `_xpc`. The same holds for `MacOS/Chromium` (173 entries), for
//!   `Helpers/chrome_crashpad_handler` (540), and for both dylibs. `strings`
//! finds no `xpc_connection_create`, and the only `xpc` in the Framework's bytes
//!   is the sandbox profile's `xpc-service-name` text and the
//!   `_OBJC_CLASS_$_NSXPCConnection` / `NSXPCInterface` **data** imports.
//! * **Chromium 130's macOS crashpad client does not use XPC either.**
//!   `breakpad_apple_util.mm` - the file the XPC handshake lived in - is gone, and
//!   so is the whole of `components/crash/core/mac/`. The client is
//!   `components/crash/core/app/crashpad_mac.mm`, which contains no `xpc`, and
//!   `third_party/crashpad/crashpad/client/crashpad_client_mac.cc`, which
//!   contains no `xpc` either. Its rendezvous is
//!   `util/mach/child_port_handshake.{h,cc}` - also no `xpc` - which reads a
//!   `child_port_token_t` and a service name out of a socketpair and then calls
//!   `BootstrapLookUp` and `child_port_check_in`.
//!
//! Those two facts agree, and the guest's import list is what the second one
//! predicts: `_bootstrap_look_up`, `_bootstrap_check_in`, `_bootstrap_port`,
//! `_bootstrap_strerror`, `_posix_spawnp`, `_waitpid`. `/run/user/1000/rosetta3/`
//! `bootstrap/` currently holds
//! `org.chromium.crashpad.child_port_handshake.165473.165473.BAXPOGUONPHNCQNZ`,
//! so that rendezvous is reached on this host today. **XPC is not on the
//! critical path, and nothing here can change that.**
//!
//! # Why the module exists anyway
//!
//! Two reasons, and neither is "to make crash reporting work".
//!
//! The first is §10.2: a bound function returning a truthful refusal beats an
//! unbound import, which is a jump through a zero slot. A guest that reached for
//! one of these would get [`xpc_connection_create`]'s refusal instead of
//! `SIGSEGV`, and a named refusal is what a reader can act on.
//!
//! The second is the sentence PLAN.md §5 step 3 item 4 actually asks for: if
//! crashpad cannot work here, **make it decline cleanly, not fake it**. This is
//! that, for the transport rather than for the exception port: a connection that
//! exists, is retained and released correctly, holds its blocks safely, and
//! reports that it has no peer.
//!
//! # The line this module does not cross
//!
//! No `xpc_object_t` is ever *sent* to a guest, and no `xpc_dictionary_t` is ever
//! built. That is the one thing that would be easy here and is not done: an
//! event is a dictionary, and the only way to hand a guest one is to invent an
//! `xpc_object_t`, whose layout `<xpc/xpc.h>` does not describe and whose
//! `_xpc_type_*` companions are exported **data** this module does not publish.
//! A guest handed a made-up dictionary takes its `xpc_get_type` answer seriously.
//! So the event handler is accepted, copied and held - and then never called,
//! because there is no event. [`connection_resume`] says so out loud instead.

#![cfg_attr(target_os = "macos", allow(dead_code))]

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Mutex, OnceLock};

use crate::dispatch::block;

/// Longest service name this host's bootstrap namespace will hold.
///
/// Taken from [`crate::mach::bootstrap`], which hex-encodes a name into a file
/// name and refuses one longer than this. Not a Darwin constant: the limit
/// belongs to launchd, and XNU's `osfmk/mach/bootstrap.h` is an empty
/// compatibility header. So this is the bound of the only namespace a lookup on
/// this host can reach, which is what makes refusing a longer name the true
/// answer rather than an arbitrary one.
const MAX_SERVICE_NAME: usize = 128;

/// Every live XPC object this host has, keyed by the handle the guest holds.
///
/// A map rather than a refcount word inside the object, and that is the whole
/// safety argument: nothing here dereferences a guest-supplied pointer, so a
/// handle this process did not mint is *answered* rather than trusted. The
/// alternative - a magic number at offset 0 - reads the guest's pointer in order
/// to find out whether reading the guest's pointer is safe.
///
/// There is no second source of XPC objects to reconcile with. libxpc is
/// macOS-only, no host library here exports one, and this map is the only
/// allocator, so every object a guest can hold is one this module made.
fn registry() -> &'static Mutex<HashMap<u64, Connection>> {
    static OBJECTS: OnceLock<Mutex<HashMap<u64, Connection>>> = OnceLock::new();
    OBJECTS.get_or_init(Default::default)
}

/// One `xpc_connection_t`.
///
/// A connection, and only a connection, because that is the only object type
/// this module can produce: `xpc_connection_create` is the only entry point
/// here that mints anything.
struct Connection {
    /// The service name, kept as its own heap buffer rather than a `String`.
    ///
    /// [`connection_get_name`] hands the guest a pointer into this, and a
    /// `HashMap` moves its values whenever it rehashes. A `CString`'s buffer
    /// stays where `malloc` put it however far the header travels; a `String`'s
    /// buffer does not.
    name: Option<CString>,

    /// The `dispatch_queue_t` from `xpc_connection_create`, held and recorded as
    /// held. Never used to run anything, because nothing is ever delivered on
    /// it - see the module docs. Kept because dropping the caller's argument on
    /// the floor would be a second, quieter lie.
    target_queue: u64,

    /// The event handler, already a `_Block_copy` of the guest's, or 0 for none.
    handler: u64,

    /// The `void *` from `xpc_connection_set_context`. A bare pointer: Darwin
    /// takes no reference on it, and neither does this.
    context: u64,

    /// Set by `xpc_connection_resume` / `activate`, cleared by `suspend`.
    active: bool,

    /// Whether [`refuse`] has already spoken about *this* connection.
    spoken: bool,

    /// Darwin's own count. A created connection is born holding one reference,
    /// which the creator's own `xpc_release` is what ends.
    references: u32,
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Deliberately not under this module's lock: `_Block_release` runs a
        // block's dispose helper, which is guest code, and guest code that
        // re-entered [`registry`] would find the lock it is being called under.
        // Every caller removes the value first and drops it after unlocking.
        block::release(self.handler);
    }
}

/// `&_xpc_type_connection`: the one type this module can honestly name, because
/// the one type it can honestly produce.
///
/// `<xpc/xpc.h>` declares the `_xpc_type_*` objects as exported **data**, and
/// this module publishes no data symbols. There is no libxpc here whose private
/// `struct _xpc_type_s` a guest could be handed, and inventing one would be a
/// guess at a private ABI - the `malloc_zone_t` bug of §7, with a type tag
/// instead of a name offset. So a guest wanting to compare types has nothing to
/// compare against, which is the true state of a host with no XPC rather than a
/// set of pointers that would compare equal to nothing.
static CONNECTION_TYPE: u8 = 0;

fn connection_type() -> u64 {
    &CONNECTION_TYPE as *const u8 as u64
}

/// Whether `handle` is a connection this process minted and has not released.
fn live(handle: u64) -> bool {
    if handle == 0 {
        return false;
    }
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(&handle)
}

/// What a connection on this host can and cannot do, said once per connection.
///
/// The refusal is worth saying out loud, because everything else in this module
/// is a `0` or a no-op and a run that only shows those reads like a crash. What
/// is true: this host has no XPC transport. There is no `xpc_connection` to
/// intercept a guest call on, no port to hand a receive right to, and no
/// implementation here of the private message format. And the service such a
/// connection would reach could not be a crash reporter in any case, because a
/// Mach exception port on this host is accepted and then never honoured - see
/// [`crate::mach::task`], which says so on the first `…_set_exception_ports`.
///
/// So the connection is real, its reference count is real, and its peer does not
/// exist. The honest report of that is a sentence, not an object.
fn refuse(caller: &str, name: Option<&CStr>) -> String {
    format!(
        "rosetta3: xpc: {caller} on connection {name}: this host has no XPC transport, so this \
         connection has no peer - no message is delivered, no reply is ever sent, and the event \
         handler is held but never called. A Mach exception port is accepted and not honoured \
         (darwin::mach::task), so a crash reporter could not be reached over this either. Refusing \
         rather than handing back an invented xpc_object_t.",
        name = match name {
            Some(name) => format!("to {name:?}"),
            None => "with no name".to_string(),
        },
    )
}

/// Mark `handle` spoken-for, and hand back the sentence if it had not been.
///
/// The lock is dropped before printing: writing to a descriptor is not cheap and
/// nothing here wants it held.
fn speak(handle: u64, caller: &str) {
    let sentence = {
        let mut objects = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match objects.get_mut(&handle) {
            Some(connection) if !connection.spoken => {
                connection.spoken = true;
                Some(refuse(caller, connection.name.as_deref()))
            }
            Some(_) => None,
            // Not a connection of ours. Said once per process, because it means
            // the guest passed a handle that was never created or has already
            // been released, and that is worth one line whatever it is.
            None => {
                static SAID: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                (!SAID.swap(true, std::sync::atomic::Ordering::Relaxed)).then(|| {
                    format!(
                        "rosetta3: xpc: {caller} was handed 0x{handle:x}, which is not a live \
                         connection on this host - never created, or already released. Answering \
                         as though it were dead rather than dereferencing it."
                    )
                })
            }
        }
    };
    if let Some(sentence) = sentence {
        eprintln!("{sentence}");
    }
}

/// `xpc_object_t xpc_retain(xpc_object_t object)`.
///
/// A real reference count on a real object, which is the part of XPC that is
/// simply correct here and the part everything else would hang off if it were
/// not. A handle this process did not mint, or one already released, answers 0
/// rather than incrementing a count that is not there.
pub unsafe extern "C" fn retain(object: u64) -> u64 {
    let mut objects = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(connection) = objects.get_mut(&object) else {
        return 0;
    };
    connection.references = connection.references.saturating_add(1);
    object
}

/// `void xpc_release(xpc_object_t object)`.
///
/// The count reaching zero ends the connection, and it ends it *once*: the value
/// leaves [`registry`] under the lock and is dropped after the lock is gone, so
/// the `_Block_release` in [`Connection::drop`] cannot deadlock against a guest
/// block that calls back in here.
pub unsafe extern "C" fn release(object: u64) {
    let dead = {
        let mut objects = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(connection) = objects.get_mut(&object) else {
            // Not ours, or already gone. Darwin makes both a programmer error;
            // doing nothing is all the damage either can do, and a second free
            // of a real object is the one failure here that cannot be recovered
            // from.
            return;
        };
        if connection.references == 0 {
            return;
        }
        connection.references -= 1;
        if connection.references == 0 {
            objects.remove(&object)
        } else {
            None
        }
    };
    drop(dead);
}

/// `xpc_type_t xpc_get_type(xpc_object_t object)`.
///
/// Correct for every object that can exist, which is the only kind: this module
/// is the sole allocator of XPC objects on this host. 0 for a handle that is not
/// live, where Darwin's contract does not say what happens and dereferencing to
/// find out is the one option that could fault.
pub unsafe extern "C" fn get_type(object: u64) -> u64 {
    live(object).then(connection_type).unwrap_or(0)
}

/// `xpc_connection_t xpc_connection_create(const char *name, dispatch_queue_t targetq)`.
///
/// A distinct, live connection - not nil, and not a stand-in for one.
///
/// The name is `_Nullable` here, and a null one is not an error: it asks for a
/// connection with no service named, which is a real thing to ask for and a real
/// thing to get. `NULL` is returned for one case only, and it is a case where
/// there is genuinely nothing to return - a non-null name this host's namespace
/// could not hold, which cannot name a service here.
///
/// The connection it returns has no peer, and the object says so rather than
/// implying otherwise; see [`refuse`].
pub unsafe extern "C" fn connection_create(name: *const c_char, target_queue: u64) -> u64 {
    // SAFETY: `name` is the caller's `_Nullable` `const char *`.
    let name = match unsafe { requested_name(name) } {
        Requested::Anonymous => None,
        Requested::Named(name) => Some(name),
        Requested::Unusable => {
            refuse_null(
                "xpc_connection_create",
                "the name it was given is not a name",
            );
            return 0;
        }
    };
    mint(name, target_queue)
}

/// `xpc_connection_t xpc_connection_create_mach_service(const char *name,
/// dispatch_queue_t targetq, uint64_t flags)`.
///
/// The Mach-service form of [`connection_create`], and the one the old XPC
/// handshake used. `flags` is carried and ignored: they select launchd
/// behaviours - privileged access, listener, and the `MACH_SERVICE` variants -
/// that have no meaning without the service, and a connection with no service
/// has nothing for them to select.
///
/// The name is not looked up. A lookup would return a send right to whatever is
/// registered and would have to be given back, and the answer would not change
/// what this connection can do: there is no XPC transport to speak over the
/// right. So the honest report is the peer's absence, which does not depend on
/// whether the name happens to be registered.
pub unsafe extern "C" fn connection_create_mach_service(
    name: *const c_char,
    target_queue: u64,
    flags: u64,
) -> u64 {
    let _ = flags;
    // SAFETY: as [`connection_create`].
    let name = match unsafe { requested_name(name) } {
        Requested::Named(name) => Some(name),
        // A null name is `XPC_NONNULL1` here, so unlike in
        // `xpc_connection_create` it names no service at all - and this variant
        // exists precisely to name one.
        Requested::Anonymous => {
            refuse_null(
                "xpc_connection_create_mach_service",
                "it was given no name at all, and this entry point declares one \
                 `XPC_NONNULL1`",
            );
            return 0;
        }
        Requested::Unusable => {
            refuse_null(
                "xpc_connection_create_mach_service",
                "the name it was given is not a name",
            );
            return 0;
        }
    };
    mint(name, target_queue)
}

/// Put a new connection into [`registry`] under a handle of its own.
///
/// The handle is a counter, not the connection's address, and that is the second
/// half of the safety argument in [`registry`]: the map owns every connection, so
/// nothing needs a handle to be dereferenceable, and a counter cannot be
/// recycled while a guest still holds it.
///
/// The counter starts at 1 because 0 is this module's answer for "no object", and
/// a handle that could be 0 could not be told from one.
fn mint(name: Option<CString>, target_queue: u64) -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let handle = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            handle,
            Connection {
                name,
                target_queue,
                handler: 0,
                context: 0,
                active: false,
                spoken: false,
                references: 1,
            },
        );
    handle
}

/// A service name out of a guest's `const char *`, in the three states the two
/// creation functions distinguish.
///
/// `Anonymous` and `Unusable` are different answers and must not be merged: a
/// null name asks for a connection with no service, which is legal and real,
/// while a name this host's namespace could not hold cannot name anything.
///
/// # Safety
///
/// `name` is null or a NUL-terminated string, which is what
/// `<xpc/connection.h>` promises for both entry points - one declares the name
/// `_Nullable` and the other `XPC_NONNULL1`.
unsafe fn requested_name(name: *const c_char) -> Requested {
    if name.is_null() {
        return Requested::Anonymous;
    }
    // SAFETY: the caller's NUL-terminated string.
    let bytes = unsafe { CStr::from_ptr(name) }.to_bytes();
    if bytes.is_empty() || bytes.len() > MAX_SERVICE_NAME {
        return Requested::Unusable;
    }
    match CString::new(bytes) {
        Ok(name) => Requested::Named(name),
        // An embedded NUL cannot reach here - `CStr` stops at the first one -
        // so this arm is unreachable rather than merely unlikely.
        Err(_) => Requested::Unusable,
    }
}

/// What [`requested_name`] found.
enum Requested {
    /// A null name.
    Anonymous,
    /// A name this host's namespace can hold.
    Named(CString),
    /// Not a name that can identify anything here.
    Unusable,
}

/// Why there is no connection to return, once per process.
///
/// The wording names both reasons on purpose, because the two callers reach this
/// for different ones: `xpc_connection_create_mach_service` for a null name as
/// well as an unusable one, and `xpc_connection_create` only for an unusable one
/// - a null name there is a legal request for a connection with no service named,
/// and it is answered rather than refused.
fn refuse_null(caller: &str, why: &str) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "rosetta3: xpc: {caller} answered NULL because {why} - empty, or longer than the \
             {MAX_SERVICE_NAME} bytes this host's bootstrap namespace holds, where a name that \
             cannot name a service has nothing to connect to. This is Darwin's own answer for an \
             unusable service name, not a substitute for one."
        );
    }
}

/// `void xpc_connection_set_target_queue(xpc_connection_t, dispatch_queue_t)`.
///
/// Held, not used: no event is ever delivered, so no queue is ever needed to
/// deliver one. Recorded rather than dropped so that the connection holds
/// everything the caller gave it.
pub unsafe extern "C" fn connection_set_target_queue(connection: u64, target_queue: u64) {
    let mut objects = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = objects.get_mut(&connection) {
        state.target_queue = target_queue;
    }
}

/// `void xpc_connection_set_event_handler(xpc_connection_t, xpc_handler_t)`.
///
/// The block is **copied**, which is the whole of the lifetime contract: Darwin
/// retains the handler for the connection's life, so the guest's block may be a
/// stack block whose frame is gone by the time anything could call it. Holding
/// the caller's pointer instead would be a use-after-free waiting for the first
/// event, and the fact that no event ever arrives here is not a defence - the
/// guest cannot know that.
///
/// A copy that cannot be taken is refused rather than aliased. See
/// [`crate::dispatch::block::copy`]: until a block runtime is registered there
/// is no `_Block_copy` to call, and the alternative is queueing a pointer into a
/// dead frame. The connection is left with no handler, which is also what a
/// connection whose handler could not be retained is.
pub unsafe extern "C" fn connection_set_event_handler(connection: u64, handler: u64) {
    if handler == 0 {
        // Clearing the handler is legal and needs no copy.
        swap_handler(connection, 0);
        return;
    }
    match block::copy(handler) {
        Some(copied) => swap_handler(connection, copied),
        None => {
            static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
                eprintln!(
                    "rosetta3: xpc: xpc_connection_set_event_handler could not copy the guest's \
                     block - no block runtime is registered, so there is no _Block_copy to call \
                     - and storing the caller's pointer would leave a reference into a frame \
                     that has returned. The connection is left with no handler."
                );
            }
        }
    }
}

/// Install `held` - already a `_Block_copy`, or 0 for none - and release whatever
/// the connection was holding.
///
/// Split out from [`connection_set_event_handler`] so that the swap can be tested
/// without a real block, and so the ordering that matters is stated once: the
/// previous handler is released *after* the lock is gone, because
/// `_Block_release` runs a block's dispose helper and that helper is guest code.
fn swap_handler(connection: u64, held: u64) {
    let previous = {
        let mut objects = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        objects
            .get_mut(&connection)
            .map(|state| std::mem::replace(&mut state.handler, held))
    };
    if let Some(previous) = previous {
        block::release(previous);
    }
}

/// `void xpc_connection_resume(xpc_connection_t connection)`.
///
/// Genuinely starts the connection, and then says what starting it amounts to.
///
/// The state change is real: created becomes active, the connection is now the
/// thing [`connection_cancel`] will find, and it has said out loud that it will
/// never deliver anything. What it does not do is deliver an event, and that is
/// not a shortcut - see the module docs. A handler that ran would have to be
/// handed an `xpc_object_t`, and the only way to produce one here is to invent
/// the type.
pub unsafe extern "C" fn connection_resume(connection: u64) {
    activate(connection, "xpc_connection_resume");
}

/// `void xpc_connection_activate(xpc_connection_t connection)`.
///
/// The 10.12 spelling of [`connection_resume`], and the same function.
pub unsafe extern "C" fn connection_activate(connection: u64) {
    activate(connection, "xpc_connection_activate");
}

/// The state change both resume and activate make.
///
/// Safe despite taking a bare handle: a handle is validated against
/// [`registry`] rather than dereferenced, so there is nothing here for a caller
/// to guarantee.
fn activate(connection: u64, caller: &str) {
    {
        let mut objects = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(state) = objects.get_mut(&connection) {
            state.active = true;
        }
    }
    speak(connection, caller);
}

/// `void xpc_connection_suspend(xpc_connection_t connection)`.
///
/// Real, and the only one of the three state changes here that can be observed
/// from outside: a suspended connection delivers nothing, and so does an active
/// one, but it is still the transition a caller is entitled to make.
pub unsafe extern "C" fn connection_suspend(connection: u64) {
    let mut objects = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = objects.get_mut(&connection) {
        state.active = false;
    }
}

/// `void xpc_connection_cancel(xpc_connection_t connection)`.
///
/// Tears the connection down, and gives the event handler back.
///
/// The second half is the part with real work in it. Darwin's contract is that
/// after this the handler "will not fire again", and a handler that provably
/// will not fire is a reference this connection no longer needs - so it is
/// released now rather than at the end of the connection's life. The
/// alternative, holding a block for a connection that has been cancelled, is a
/// leak a caller cannot see.
pub unsafe extern "C" fn connection_cancel(connection: u64) {
    let previous = {
        let mut objects = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        objects.get_mut(&connection).map(|state| {
            state.active = false;
            std::mem::take(&mut state.handler)
        })
    };
    if let Some(previous) = previous {
        block::release(previous);
    }
}

/// `void xpc_connection_send_message(xpc_connection_t, xpc_object_t)`.
///
/// The message goes nowhere, and the function says so rather than returning as
/// though it had.
///
/// It is **not** released. `<xpc/connection.h>` is explicit that the connection
/// "logically copies" the dictionary, so the caller keeps its reference and may
/// go on using the message afterwards; releasing it here would be a release the
/// caller never authorised, and a double free one line later.
pub unsafe extern "C" fn connection_send_message(connection: u64, message: u64) {
    let _ = message;
    speak(connection, "xpc_connection_send_message");
}

/// `void xpc_connection_send_message_with_reply(xpc_connection_t, xpc_object_t,
/// dispatch_queue_t replyq, xpc_handler_t handler)`.
///
/// The reply handler is **never called**, and that is the answer rather than a
/// missing feature: there is no peer, so there is no reply, and a handler that
/// ran would be reporting a message that no process received.
///
/// This is the call that loses a start-up when it is answered wrongly. A client
/// waiting on a reply that never comes blocks forever; a client handed a
/// synthesised reply acts on a message nobody sent. Neither happens here: the
/// message keeps the caller's reference, the reply queue and the handler are
/// left alone, and the connection says what it is.
///
/// The handler is deliberately not copied. libxpc would retain it for the
/// lifetime of the reply, and there is no reply, so retaining it would be holding
/// a reference to a callback that provably will not run. The block is the
/// caller's to release, as it was on the way in.
pub unsafe extern "C" fn connection_send_message_with_reply(
    connection: u64,
    message: u64,
    reply_queue: u64,
    handler: u64,
) {
    let _ = (message, reply_queue, handler);
    speak(connection, "xpc_connection_send_message_with_reply");
}

/// `xpc_object_t xpc_connection_send_message_with_reply_sync(xpc_connection_t,
/// xpc_object_t)`.
///
/// `0`, immediately.
///
/// This is the one that would hang. The header's own warning - "it can block
/// indefinitely" - is the whole hazard, and `0` is the answer Darwin itself
/// gives when the connection went invalid before the message could be sent.
/// Blocking here for a reply that cannot arrive is the failure this module
/// exists to avoid.
///
/// The caller owns the returned object and there is none, so there is nothing to
/// release: `xpc_release(0)` is a no-op in this module as it is in libxpc.
pub unsafe extern "C" fn connection_send_message_with_reply_sync(
    connection: u64,
    message: u64,
) -> u64 {
    let _ = message;
    speak(connection, "xpc_connection_send_message_with_reply_sync");
    0
}

/// `const char *xpc_connection_get_name(xpc_connection_t)`.
///
/// The name the caller gave, or `NULL`.
///
/// `NULL` is one of the two answers the header documents, and it is the right one
/// for a connection created without a name. The returned pointer is into the
/// connection's own allocation and stays valid until the connection is released;
/// a caller that outlives the connection is a caller that has already gone wrong.
pub unsafe extern "C" fn connection_get_name(connection: u64) -> *const c_char {
    let objects = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(state) = objects.get(&connection) else {
        return std::ptr::null();
    };
    match state.name.as_ref() {
        Some(name) => name.as_ptr(),
        None => std::ptr::null(),
    }
}

/// `pid_t xpc_connection_get_pid(xpc_connection_t)`.
///
/// `0`: there is no remote process, and `0` is how a process id says so. Not a
/// guess at a plausible pid - a connection with no peer has no peer to guess at.
pub unsafe extern "C" fn connection_get_pid(connection: u64) -> i32 {
    let _ = connection;
    0
}

/// `uid_t xpc_connection_get_euid(xpc_connection_t)`.
///
/// `0`, for the same reason as [`connection_get_pid`]: the effective user id of
/// the remote peer, and there is no remote peer. It is not this process's own
/// euid, which would be a real number standing in for a question that was not
/// asked.
pub unsafe extern "C" fn connection_get_euid(connection: u64) -> u32 {
    let _ = connection;
    0
}

/// `gid_t xpc_connection_get_egid(xpc_connection_t)`.
///
/// `0`, as [`connection_get_euid`].
pub unsafe extern "C" fn connection_get_egid(connection: u64) -> u32 {
    let _ = connection;
    0
}

/// `au_asid_t xpc_connection_get_asid(xpc_connection_t)`.
///
/// `0`, as [`connection_get_euid`]: the audit session of a peer that is not
/// there.
pub unsafe extern "C" fn connection_get_asid(connection: u64) -> u32 {
    let _ = connection;
    0
}

/// `void xpc_connection_set_context(xpc_connection_t, void *)`.
///
/// A bare pointer, stored. Darwin takes no reference on a context and neither
/// does this, so the guest keeps owning whatever it pointed at - which is why the
/// header tells a caller managing the memory to set a finalizer, and why not
/// pretending to own it here is the correct answer rather than a leak.
pub unsafe extern "C" fn connection_set_context(connection: u64, context: u64) {
    let mut objects = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = objects.get_mut(&connection) {
        state.context = context;
    }
}

/// `void *xpc_connection_get_context(xpc_connection_t)`.
///
/// What was set, or `0`.
pub unsafe extern "C" fn connection_get_context(connection: u64) -> *mut c_void {
    let objects = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match objects.get(&connection) {
        Some(state) => state.context as *mut c_void,
        None => std::ptr::null_mut(),
    }
}

/// `xpc_connection_t xpc_dictionary_get_remote_object(xpc_object_t)` does not
/// exist.
///
/// It was the pre-10.7 spelling, replaced by the pair below; a brief that names
/// it is naming an API fourteen years older than the guest. Both halves are here
/// and both answer `0`, because a remote connection is something that arrives
/// *inside* a received message, and no message is ever received here.
pub unsafe extern "C" fn dictionary_get_remote_connection(dictionary: u64) -> u64 {
    let _ = dictionary;
    0
}

/// `xpc_connection_t xpc_dictionary_create_connection(xpc_object_t, const char *)`.
///
/// `0`: the key names a connection in a dictionary this process never received.
/// There is nothing to extract, and inventing one would be a connection to a
/// process that does not exist - which is the `xpc_object_t` this module refuses
/// to fabricate.
pub unsafe extern "C" fn dictionary_create_connection(dictionary: u64, key: *const c_char) -> u64 {
    let _ = (dictionary, key);
    0
}

/// `xpc_connection_t xpc_array_create_connection(xpc_object_t, size_t)`.
///
/// `0`, as [`dictionary_create_connection`].
pub unsafe extern "C" fn array_create_connection(array: u64, index: u64) -> u64 {
    let _ = (array, index);
    0
}

/// Empty on macOS, where the host's own libxpc answers these.
#[cfg(target_os = "macos")]
exports::exports! {}

#[cfg(not(target_os = "macos"))]
exports::exports! {
    functions {
        // The object lifetime and type, which are simply correct here.
        "xpc_retain" => retain,
        "xpc_release" => release,
        "xpc_get_type" => get_type,

        // Connection lifetime.
        "xpc_connection_create" => connection_create,
        "xpc_connection_create_mach_service" => connection_create_mach_service,
        "xpc_connection_set_target_queue" => connection_set_target_queue,
        "xpc_connection_set_event_handler" => connection_set_event_handler,
        "xpc_connection_activate" => connection_activate,
        "xpc_connection_suspend" => connection_suspend,
        "xpc_connection_resume" => connection_resume,
        "xpc_connection_cancel" => connection_cancel,

        // Sending, and the two answers that stop a start-up hanging.
        "xpc_connection_send_message" => connection_send_message,
        "xpc_connection_send_message_with_reply" => connection_send_message_with_reply,
        "xpc_connection_send_message_with_reply_sync" => connection_send_message_with_reply_sync,

        // The peer's identity, of which there is none.
        "xpc_connection_get_name" => connection_get_name,
        "xpc_connection_get_pid" => connection_get_pid,
        "xpc_connection_get_euid" => connection_get_euid,
        "xpc_connection_get_egid" => connection_get_egid,
        "xpc_connection_get_asid" => connection_get_asid,

        // The context, held as the bare pointer Darwin's contract says it is.
        "xpc_connection_set_context" => connection_set_context,
        "xpc_connection_get_context" => connection_get_context,

        // The "remote object" accessors, under the names they have had since
        // 10.7. See `dictionary_get_remote_connection`.
        "xpc_dictionary_get_remote_connection" => dictionary_get_remote_connection,
        "xpc_dictionary_create_connection" => dictionary_create_connection,
        "xpc_array_create_connection" => array_create_connection,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    /// A name a test can hand over, alive for the call.
    fn name(text: &str) -> CString {
        CString::new(text).unwrap()
    }

    /// A connection with a fresh handle, released when the test ends.
    ///
    /// Every test makes its own rather than sharing one, because [`registry`] is
    /// process-wide and these run in parallel.
    struct Handle(u64);

    impl Handle {
        fn named(text: &str) -> Self {
            // SAFETY: `text` outlives the call, and the string is NUL-terminated.
            Self(unsafe { connection_create(name(text).as_ptr(), 0) })
        }

        fn live(&self) -> bool {
            live(self.0)
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from `connection_create` and is released
            // exactly once, here.
            unsafe { release(self.0) };
        }
    }

    /// The point of the module: a connection is a real, distinct, live object -
    /// not nil, and not a stand-in - and it says it has no type beyond the one
    /// it can honestly be.
    #[test]
    fn a_created_connection_is_a_live_object_of_its_own() {
        let first = Handle::named("com.example.first");
        let second = Handle::named("com.example.second");
        assert_ne!(first.0, 0, "a usable name must not produce NULL");
        assert_ne!(first.0, second.0, "two connections are two objects");
        assert!(first.live() && second.live());
        // SAFETY: a null handle is answered without a dereference.
        unsafe {
            assert_ne!(get_type(first.0), 0);
            assert_eq!(get_type(first.0), get_type(second.0));
        }
    }

    /// Nothing here is reachable by this guest, so a handle that never existed is
    /// the interesting case: it must be answered, not dereferenced.
    #[test]
    fn a_handle_that_was_never_minted_is_answered_rather_than_trusted() {
        // SAFETY: every one of these is a handle this module did not mint, which
        // is exactly what the functions are documented to survive.
        unsafe {
            assert_eq!(get_type(0), 0);
            assert_eq!(get_type(0x1000), 0, "a stack address is not an object");
            assert_eq!(retain(0x1000), 0);
            assert_eq!(connection_get_pid(0x1000), 0);
            assert!(connection_get_name(0x1000).is_null());
            assert!(connection_get_context(0x1000).is_null());
        }
    }

    /// The reference count is a real count, and reaching zero ends the object
    /// exactly once - the failure a leak or a double free would both show up in.
    #[test]
    fn a_retained_connection_outlives_its_creator() {
        // SAFETY: every call here is on a handle this test owns.
        unsafe {
            let connection = connection_create(name("com.example.counted").as_ptr(), 0);
            assert!(live(connection));

            assert_eq!(retain(connection), connection, "retain returns the object");
            release(connection);
            assert!(live(connection), "one reference is still held");

            release(connection);
            assert!(!live(connection), "the last reference ends it");

            // And a second release is a no-op rather than a second free.
            release(connection);
            assert!(!live(connection));
        }
    }

    /// A name this host's bootstrap namespace could not hold is not a name that
    /// can identify a service, so there is no connection to return. The one
    /// genuine `NULL` this module produces.
    ///
    /// A *null* name is a different question and has a different answer, which
    /// is why this test pins both: `xpc_connection_create` declares its name
    /// `_Nullable` and a null one is legal, while `xpc_connection_create_mach_service`
    /// declares it `XPC_NONNULL1` and exists precisely to name a service.
    #[test]
    fn a_name_the_namespace_could_not_hold_is_refused() {
        let long = "c".repeat(MAX_SERVICE_NAME + 1);
        // SAFETY: every string below outlives the call it is passed to.
        unsafe {
            // Over the limit, and empty: not names at all.
            assert_eq!(connection_create_mach_service(name(&long).as_ptr(), 0, 0), 0);
            assert_eq!(connection_create_mach_service(name("").as_ptr(), 0, 0), 0);
            assert_eq!(connection_create(name(&long).as_ptr(), 0), 0);
            assert_eq!(connection_create(name("").as_ptr(), 0), 0);

            // At the limit is still a name.
            let limit = "c".repeat(MAX_SERVICE_NAME);
            let at_limit = connection_create_mach_service(name(&limit).as_ptr(), 0, 0);
            assert_ne!(at_limit, 0);
            release(at_limit);

            // And a null name: refused by the variant that requires one, honoured
            // by the one that does not.
            assert_eq!(connection_create_mach_service(std::ptr::null(), 0, 0), 0);
            let anonymous = connection_create(std::ptr::null(), 0);
            assert_ne!(anonymous, 0, "a null name is a connection, not a refusal");
            release(anonymous);
        }
    }

    /// The name comes back, and a connection made without one answers `NULL` -
    /// which the header lists as a legitimate result rather than a failure.
    #[test]
    fn a_name_is_answered_and_a_missing_one_is_not() {
        // SAFETY: every call is on a handle this test owns or has released.
        unsafe {
            let named = connection_create(name("com.example.named").as_ptr(), 0);
            let returned = CStr::from_ptr(connection_get_name(named));
            assert_eq!(returned, CStr::from_bytes_with_nul(b"com.example.named\0").unwrap());

            let anonymous = connection_create(std::ptr::null(), 0);
            assert_ne!(anonymous, 0, "no name is not no connection");
            assert!(connection_get_name(anonymous).is_null());

            release(named);
            release(anonymous);
        }
    }

    /// The peer questions, all of which have the same true answer: there is no
    /// remote process, so there is nothing to report about it. Notably not this
    /// process's own pid, which would be a real number answering a question
    /// nobody asked.
    #[test]
    fn a_connection_reports_no_peer() {
        let connection = Handle::named("com.example.unpeered");
        // SAFETY: the handle is live for the whole test.
        unsafe {
            assert_eq!(connection_get_pid(connection.0), 0);
            assert_eq!(connection_get_euid(connection.0), 0);
            assert_eq!(connection_get_egid(connection.0), 0);
            assert_eq!(connection_get_asid(connection.0), 0);
        }
    }

    /// A context is a bare pointer, in and out, and the guest keeps owning it.
    #[test]
    fn a_context_is_held_as_the_bare_pointer_it_is() {
        let connection = Handle::named("com.example.context");
        let mut owned = 0x1234u64;
        // SAFETY: `owned` outlives the connection, and the handle is live.
        unsafe {
            let slot = &raw mut owned as *mut u64 as u64;
            connection_set_context(connection.0, slot);
            assert_eq!(connection_get_context(connection.0) as u64, slot);
            connection_set_context(connection.0, 0);
            assert!(connection_get_context(connection.0).is_null());
        }
    }

    /// The event handler is installed and taken back by copy, and clearing it
    /// needs no copy at all.
    ///
    /// The copy itself is [`crate::dispatch::block::copy`]`, which has its own
    /// tests, and it is not exercised from here: whether a block runtime is
    /// registered during a `darwin` unit-test run is process-wide state that
    /// `dispatch`'s tests also set, so a test that assumed "no runtime" would be
    /// asserting on another module's timing. Worse, it would have to hand
    /// `block::copy` a pointer that is not a block - and with a runtime
    /// registered, `_Block_copy` would dereference it.
    #[test]
    fn an_event_handler_is_installed_and_cleared_through_one_slot() {
        let connection = Handle::named("com.example.handler");
        // SAFETY: `swap_handler` releases whatever it displaces through
        // `block::release`, which is a no-op for 0, and both values here are 0 -
        // no pointer that is not a block reaches the block runtime.
        unsafe {
            swap_handler(connection.0, 0);
            assert_eq!(handler_of(connection.0), 0);
            connection_set_event_handler(connection.0, 0);
            assert_eq!(handler_of(connection.0), 0, "clearing needs no copy");
        }
    }

    /// A name handed out by `xpc_connection_get_name` outlives the map moving its
    /// connection around.
    ///
    /// The `CString` is not a detail: a `HashMap` rehashes and moves its values,
    /// so a `String` header would travel and its buffer would not, and the
    /// pointer a guest holds would be left describing nothing. This creates
    /// enough connections to force several rehashes and checks the name after.
    #[test]
    fn a_name_survives_the_registry_moving_its_connection() {
        // SAFETY: every connection is released at the end of the test, once.
        unsafe {
            let mut handles = Vec::new();
            for index in 0..64 {
                let text = format!("com.example.rehash{index}");
                handles.push((connection_create(name(&text).as_ptr(), 0), text));
            }
            for (handle, text) in &handles {
                assert!(
                    live(*handle),
                    "a rehash must not lose a connection: {text}"
                );
                assert_eq!(
                    CStr::from_ptr(connection_get_name(*handle)).to_bytes(),
                    text.as_bytes(),
                    "{text}: the name must survive the map moving it"
                );
            }
            for (handle, _) in handles {
                release(handle);
            }
        }
    }

    /// The two sends that lose a start-up, answered the way Darwin answers a
    /// connection that went invalid: immediately, and with nothing.
    #[test]
    fn no_reply_is_ever_synthesised() {
        let connection = Handle::named("com.example.reply");
        // SAFETY: the handle is live. The message pointer is never dereferenced:
        // these functions look at neither argument, because a message this host
        // cannot encode is not a message it can also decode.
        unsafe {
            connection_send_message(connection.0, 0);
            connection_send_message_with_reply(connection.0, 0, 0, 0);
            assert_eq!(
                connection_send_message_with_reply_sync(connection.0, 0),
                0,
                "a synchronous reply must be absent, not awaited"
            );
        }
    }

    /// `xpc_dictionary_get_remote_object` is the pre-10.7 name for the pair
    /// below. Nothing is ever received, so there is no remote object to get.
    #[test]
    fn a_remote_object_is_never_invented() {
        // SAFETY: none of these dereferences its argument; all three answer from
        // the fact that no message has been received.
        unsafe {
            assert_eq!(dictionary_get_remote_connection(0x1000), 0);
            assert_eq!(dictionary_create_connection(0x1000, std::ptr::null()), 0);
            assert_eq!(array_create_connection(0x1000, 0), 0);
        }
    }

    /// Resume and suspend are real state changes, and cancel is real teardown.
    #[test]
    fn resume_suspend_and_cancel_are_real_state_changes() {
        // SAFETY: every call is on a handle this test owns.
        unsafe {
            let connection = connection_create(name("com.example.state").as_ptr(), 0);
            assert!(!active(connection), "a connection is not live until resumed");
            connection_resume(connection);
            assert!(active(connection));
            connection_suspend(connection);
            assert!(!active(connection));
            connection_activate(connection);
            assert!(active(connection));
            connection_cancel(connection);
            assert!(!active(connection));
            // Cancel does not deallocate: the caller's reference is still its own.
            assert!(live(connection));
            release(connection);
            assert!(!live(connection));
        }
    }

    /// Whether a connection is active, for the test above.
    fn active(connection: u64) -> bool {
        registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&connection)
            .is_some_and(|state| state.active)
    }

    /// The handler a connection currently holds, for the test above.
    fn handler_of(connection: u64) -> u64 {
        registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&connection)
            .map_or(0, |state| state.handler)
    }
}
