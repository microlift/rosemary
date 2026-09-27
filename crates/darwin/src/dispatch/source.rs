#![cfg(not(target_os = "macos"))]
//! `dispatch_source_*`: a Mach receive source, backed by the port's own queue.
//!
//! ## What the guest asks for
//!
//! **The census, and it is small.** `dispatch_source_create` has **four** call
//! sites in the 214 MB of `Chromium Framework` text, and the type argument at
//! each is a `mov rdi, QWORD PTR [rip+…]` naming a `__dispatch_source_type_*`
//! import. There are exactly **three** such imports in the whole 244 MB image -
//! `__dispatch_source_type_mach_recv`, `__dispatch_source_type_proc` and
//! `__dispatch_source_type_memorypressure` - so every site is accounted for and
//! no fourth type exists to account for.
//!
//! | where | type (GOT slot) | what the guest does with the source | waited on? |
//! |---|---|---|---|
//! | `+0x5dae491` | `mach_recv` `0xdd50850` | stored in the watcher; the event handler cancels the source from inside itself | **yes** - the watcher's whole purpose |
//! | `+0x5d927ed` | `mach_recv` `0xdd50850` | stored; a **cancel** handler signals a semaphore the destructor waits on | **yes** - `dispatch_semaphore_wait(sem, DISPATCH_TIME_FOREVER)` |
//! | `+0x3b3b73f` | `memorypressure` `0xdd50568` | stored at `+0x18`, then **null-checked** and skipped if null | no |
//! | `+0x5da33a6` | `proc` `0xdd508c0` | stored in a `std::map<pid_t, ClientData>`; **no null check anywhere** | no |
//!
//! **The two that are waited on are the two this file implements.** That is the
//! whole of why a `NULL` here is survivable rather than a hang, and it is worth
//! stating as the property rather than leaving it to be re-derived per site:
//!
//! * `+0x3b3b73f` - `+0x3b3b8a5  mov rdi,[rbx+0x18]` / `+0x3b3b8a9  test rdi,rdi`
//!   / `+0x3b3b8ac  je 0x3b3b94d`, so a `NULL` source skips both the handler and
//!   the resume. `SystemMemoryPressureEvaluator`'s only other input is the
//!   `kern.memorystatus_vm_pressure_level` `sysctl`, which this runtime answers
//!   `ENOENT`, so the evaluator reports "normal" for the life of the process.
//!   That is the true state of a host with no memory-pressure condition to
//!   report, not a fiction.
//! * `+0x5da33a6` - `MachPortRendezvousServer::RegisterPortsForPid` in
//!   `base/mac/mach_port_rendezvous.cc`, called from `LaunchProcess`
//!   (`+0x5dab23a`, whose `__FILE__` is `base/process/launch_mac.cc` and whose
//!   string argument is `"LaunchProcess"`). It installs a handler that calls
//!   `OnClientExited(pid)` and **waits for nothing**: no semaphore, no condvar,
//!   no second pass. The only consequence of the `NULL` is that the Mach rights
//!   a child was launched with are never `Destroy()`ed, which is a leak until
//!   the process exits and is not on the path to anything. Its null-safety is
//!   visible in the guest's own code: `+0x5da341f`/`+0x5da3423` and
//!   `+0x5da3556`/`+0x5da3559` test the handle before `dispatch_retain` and
//!   `dispatch_release`, and every function here returns early for a null
//!   handle, so no null ever reaches an allocator.
//! * `+0x5d927ed` is the one that would hang, and it cannot: it is a
//!   `mach_recv` over a real receive right, so the only way to get a `NULL` is
//!   `refuse_port` - a port name that does not resolve in this task's space -
//!   which already announces itself per call rather than once.
//!
//! ### There is no timer source in this guest, and that is measured
//!
//! `__dispatch_source_type_timer`, `dispatch_timer_create`,
//! `dispatch_source_set_timer`, `dispatch_source_get_type`,
//! `dispatch_source_get_data`, `dispatch_source_get_handle`,
//! `dispatch_source_get_mask`, `dispatch_source_merge_data` and
//! `dispatch_source_testcancel` are each **zero occurrences** in the image -
//! not bound, not named in either of the two symbol-name tables, not present as
//! bytes. Five controls scanned in the same pass each fire twice, once in the
//! name table at `0xe74xxxx` and once in the one at `0xe76fxxxx`:
//! `_dispatch_source_create`, `_dispatch_semaphore_wait`, `_dispatch_resume`,
//! `_notify_register_dispatch` and `__dispatch_source_type_mach_recv`.
//!
//! So there is no `DISPATCH_SOURCE_TYPE_TIMER` to wire to the `timerfd` that
//! `crate::mach::kevent64` already uses for `EVFILT_TIMER`. Chromium on macOS
//! does its timing through `CFRunLoopTimerCreate` and `dispatch_after`, both of
//! which it imports and both of which are answered here. Building a timer
//! source would be answering a question this guest never asks, and a source
//! type with no call site is a source type nobody has checked against one.
//!
//! The first site is the one on Chromium's critical path, and its call is this:
//!
//! ```text
//!  5dae47b  xor    edi,edi                    ; DISPATCH_QUEUE_PRIORITY_DEFAULT
//!  5dae47d  xor    esi,esi
//!  5dae47f  call   0xcca0f2a                 ; _dispatch_get_global_queue
//!  5dae482  mov    rdi,QWORD PTR [rip+…]     ; 0xdd50850 = __dispatch_source_type_mach_recv
//!  5dae489  mov    rsi,r12                   ; r12d = receive_right_->Name()
//  5dae48c  xor    edx,edx                   ; mask
//!  5dae48e  mov    rcx,rax                   ; the queue
//!  5dae491  call   0xcca0f24                 ; _dispatch_source_create
//!  5dae557  lea    rsi,[rbp-0x70]            ; the handler block, on the stack
//!  5dae55b  call   0xcca0f1e                 ; _dispatch_source_set_event_handler
//!  5dae567  call   0xcca0d56                 ; _dispatch_resume
//! ```
//!
//! `0xdd50850` and `0xdd50160` (`__NSConcreteStackBlock`) resolve through
//! `__DATA_CONST,__got` with `macho.py`'s `ist_name`, and `r12d` is
//! `DWORD PTR [[this+8]+0x10]` - the `mach_port_t` the watcher's receive right is
//! named by, which the same function also hands the handler block as its capture
//! at `+0x40`.
//!
//! **That the second argument is the port is not a guess about the guest.** It is
//! what libdispatch's own prototype says: `src/source.c` in
//! `apple-oss-distributions/libdispatch` declares
//!
//! ```c
//! dispatch_source_create(dispatch_source_type_t dst, uintptr_t handle,
//!                         uintptr_t mask, dispatch_queue_t dq)
//! ```
//!
//! The header's `(type, mask, handle, queue)` and the implementation's
//! `(type, handle, mask, queue)` name the same two parameters the other way
//! round, and `dux_create(dst, handle, mask)` is what receives them. Chromium
//! passing `receive_right_->Name()` second is the ordinary call; reading it as the
//! mask would be the error.
//!
//! The handler is a **block**, built on the caller's stack with
//! `flags == 0x46000000` - `BLOCK_HAS_COPY_DISPOSE | BLOCK_HAS_CTOR |
//! BLOCK_HAS_SIGNATURE` - and its captures (`dispatch_source_t`,
//! `scoped_refptr<SequencedTaskRunner>`, `BindOnce`, `WeakPtr`, `mach_port_t`) are
//! copied in after the literal. It is invoked on the source's target queue, on a
//! `dispatch` pool worker, with the block's `invoke` already a thunk: the same
//! path `dispatch_async` takes, and for the same reason - the handler outlives
//! the call that installed it.
//!
//! ## The mechanism
//!
//! A Mach receive right in [`crate::mach::wire`] is a `SOCK_DGRAM` unix socket
//! bound to a private abstract name, so the source's event condition is
//! *literally* `poll` on that descriptor reporting a datagram. That is the same
//! condition [`crate::mach::kevent64`] waits on for `EVFILT_MACHPORT` and the
//! same one [`crate::mach::msg::mach_port_peek`] answers `KERN_SUCCESS` for, so
//! the three agree by construction rather than by convention: a source fires
//! exactly when `mach_msg` would return a message and `mach_port_peek` would
//! answer success.
//!
//! One thread for every source, not a thread per source: it snapshots the armed
//! sources, `poll`s their descriptors together with a self-pipe, and submits each
//! ready source's handler to that source's own target queue. The pipe is what an
//! arm or a cancel writes to, so a source added or removed while the thread is
//! blocked wakes it rather than waiting for a message that may never come.
//!
//! Delivery is level-triggered, as Darwin's is: a message is still at the head of
//! the port until somebody receives it, so a handler that does not drain the port
//! fires again on the next pass. An armed source stays in the watcher's `poll` set
//! for its whole life, so the re-arm **is** the next `poll` and nothing else -
//! which is what makes the level-triggered behaviour cost one `poll` per pass rather
//! than a delivery that was never tested for readiness. Chromium's handler cancels
//! the source from inside itself - "A watcher is one-shot" - so for this guest that
//! is one delivery and then nothing. A delivery already on its way to the queue is
//! not queued twice ([`State::firing`]), which is the coalescing Darwin's
//! `ds_pending_data` does.
//!
//! One delivery more than there were messages is possible, and is not a defect
//! here. `poll` does not hold a descriptor's lock across both its readiness check
//! and its return, so it may report a datagram that another thread took in
//! between; the handler then finds nothing at the head of the queue. `kqueue`
//! permits the same thing, so a source on Darwin can be delivered to once more
//! than there were messages, and every poll-driven loop is written to tolerate it.
//! What this module does *not* permit is the loop that would follow: the next
//! `poll` sees an empty port and the source goes quiet.
//!
//! ## Three semantics that are easy to get wrong
//!
//! * **A cancel handler runs on a source that was never resumed.**
//!   `dispatch_source_cancel` sets `DSF_CANCELED` and wakes the source; a source
//!   that was never activated goes through `_dispatch_source_activate`, which
//!   marks it deleted, and `_dispatch_source_invoke2` then reaches
//!   `_dispatch_source_cancel_callout` anyway. The `base::apple` teardown at
//!   `+0x5d928b0` depends on it: it cancels, then blocks on a semaphore that only
//!   the cancel handler signals, so getting this wrong is a deadlock rather than a
//!   missed callback.
//! * **A second `dispatch_source_cancel` is a no-op.** `dispatch_source_cancel`
//!   returns early when the flag is already set, so the handler does not run
//!   twice. Chromium reaches cancel from two places - the event handler and
//!   `StopWatching` - on the same source.
//! * **The event handler is released at cancel, not at the last release.**
//!   `_dispatch_source_cancel_callout` frees the event and registration handlers
//!   before it calls out, which is what drops the block's `BindOnce` and its
//!   `scoped_refptr` capture on the way to the cancel handler.
//!
//! ## What is refused, and why that is an answer
//!
//! Every source type but `MACH_RECV`. `dispatch_source_create` answers `NULL`,
//! which is what Darwin answers for a type it does not recognise, and says which
//! type it was given and that only the Mach receive one is implemented. The two
//! the guest actually uses besides it are named rather than left to be found:
//!
//! * `DISPATCH_SOURCE_TYPE_PROC` (`+0x5da33a6`, mask the child's pid, handle
//!   `NOTE_EXITSTATUS`) is a knote on a pid's exit status. **This one is
//!   answerable and is not answered**, because the honest cost is a `pidfd` in
//!   this module's `poll` set and the guest does not wait on it; see the table
//!   above for why the `NULL` costs a port leak and not a hang. The refusal is
//!   correct as an answer and incomplete as a feature.
//! * `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE` (`+0x3b3b73f`, mask `0x7`) is a knote
//!   on the kernel's memory-pressure notifications. **The reason is sharper than
//!   "it would be a second project", and it is worth stating precisely because
//!   `/proc/pressure/memory` exists on this host** (Linux 7.2.6) and does carry
//!   `some`/`full` `avg10`/`avg60`/`avg300` and a monotonic `total` in
//!   microseconds. What it does not carry is `DISPATCH_MEMORYPRESSURE_WARN` or
//!   `DISPATCH_MEMORYPRESSURE_CRITICAL`: those are a macOS kernel *policy*, and
//!   the guest's mask is `0x7` - all three levels - so any level this runtime
//!   reported would be a threshold this file invented, and an invented level is
//!   the one answer the guest would act on. The condition is pollable and the
//!   *value* is not derivable, and the guest wants the value.
//!
//! The type *tokens* for all three are still published, as the distinct stable
//! addresses they are in libdispatch: a guest that compares two of them, or that
//! reads one and branches on it being non-null, gets the truth. The refusal is in
//! `dispatch_source_create`, where the source would be made.

use std::ffi::c_void;
use std::sync::{Arc, Mutex, OnceLock};

use super::block;
use super::object::{self, Function, Header, Kind};
use super::queue;
use crate::mach::space::{self, Name};
use crate::mach::wire::Receiver;

// ----------------------------------------------------------------- source types

/// A `struct dispatch_source_type_s`.
///
/// libdispatch's are real objects, and a caller only ever takes their address and
/// hands it back to `dispatch_source_create`. So what has to be right about them
/// is that the addresses are **distinct** and **not null**, and this is that: one
/// `static` per source type, each carrying its own name.
///
/// The payload is not decoration. A zero-sized `static`'s address is the
/// compiler's choice and two of them are free to be the same one - which is
/// exactly what happened the first time this was three empty types, and it is the
/// kind of bug a test written alongside the mistake does not catch.
struct TypeToken {
    /// `DISPATCH_SOURCE_TYPE_*`, for the refusal `create` prints.
    name: &'static str,
    /// Whether this runtime has a mechanism behind the type, which is the whole
    /// of what [`create`] decides on.
    implemented: bool,
}

static MACH_RECV: TypeToken = TypeToken {
    name: "DISPATCH_SOURCE_TYPE_MACH_RECV",
    implemented: true,
};
static PROC: TypeToken = TypeToken {
    name: "DISPATCH_SOURCE_TYPE_PROC",
    implemented: false,
};
static MEMORYPRESSURE: TypeToken = TypeToken {
    name: "DISPATCH_SOURCE_TYPE_MEMORYPRESSURE",
    implemented: false,
};

/// The address of `token`, which is the whole of what a caller does with one.
fn address_of<T>(token: &'static T) -> u64 {
    &raw const *token as u64
}

/// The token `address` is, or `None` for one this file does not have.
///
/// Every comparison of a source type goes through here rather than comparing
/// `u64`s at the call site, so the set of types this runtime knows and the set it
/// implements are one list read from one place.
fn token_for(address: u64) -> Option<&'static TypeToken> {
    if address == mach_recv_token() {
        Some(&MACH_RECV)
    } else if address == proc_token() {
        Some(&PROC)
    } else if address == memorypressure_token() {
        Some(&MEMORYPRESSURE)
    } else {
        None
    }
}

/// `__dispatch_source_type_mach_recv`, the one this runtime implements.
pub fn mach_recv_token() -> u64 {
    address_of(&MACH_RECV)
}

/// `__dispatch_source_type_proc`, which [`create`] refuses.
pub fn proc_token() -> u64 {
    address_of(&PROC)
}

/// `__dispatch_source_type_memorypressure`, which [`create`] refuses.
pub fn memorypressure_token() -> u64 {
    address_of(&MEMORYPRESSURE)
}

/// The name of a type token, for the diagnostic [`create`] prints.
///
/// libdispatch has one token per source type and there are more than the three
/// here, so a guest asking for a fourth is reported by address rather than by a
/// name this runtime would have to invent.
fn type_name(address: u64) -> &'static str {
    match token_for(address) {
        Some(token) => token.name,
        None => "a source type this runtime does not know",
    }
}

// ---------------------------------------------------------------------- object

/// What a source watches. One arm, because one type is implemented.
enum Filter {
    /// `DISPATCH_SOURCE_TYPE_MACH_RECV`: the port, or the members of a port set.
    MachRecv,
}

/// `#[repr(C)]` and the header first, for the reason every object in this
/// directory has it: [`super::object`] recovers a `Source` by casting a
/// `*mut Header`.
#[repr(C)]
pub struct Source {
    /// Read through a `*mut Header` rather than through this field, which is what
    /// makes it look unused: it must be first, and its bytes are the object
    /// protocol's.
    #[allow(dead_code)]
    header: Header,
    /// Never read, because a source is only ever made as a `MachRecv` and
    /// [`Filter::MachRecv`] is the only arm. Kept so the type is the reason the
    /// object exists rather than an accident of it.
    #[allow(dead_code)]
    filter: Filter,
    /// The port name. Kept rather than re-derived, because a name this namespace
    /// recycles would otherwise be indistinguishable from this port.
    port: Name,
    /// Where the handlers run, and the reference the source holds on it: a
    /// handler submitted to a released queue is a use-after-free, and the guest
    /// has no way to know that is what happened.
    queue: u64,
    state: Mutex<State>,
}

struct State {
    event: Handler,
    cancel: Handler,
    /// `DSF_CANCELED`.
    cancelled: bool,
    /// Sources are created inactive, so this starts at one and `dispatch_resume`
    /// is what takes it to zero. A count rather than a flag because
    /// `dispatch_suspend` on an already-suspended object is a no-op on Darwin, and
    /// two suspends need two resumes.
    suspends: u32,
    /// A delivery for this source is already on its way to the target queue.
    firing: bool,
}

/// One of a source's two handlers, in either of the two spellings libdispatch
/// takes them in.
///
/// Exactly one of the two is ever set: a block handler is a heap copy this module
/// owns and releases, and a `dispatch_function_t` is a bare function pointer
/// called with the source's context. Recording which is which is what lets the
/// clear-a-handler case (`dispatch_source_set_event_handler(ds, NULL)`) release
/// the block and not the function.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct Handler {
    block: u64,
    function: u64,
}

impl Handler {
    /// Take the handler out, leaving nothing behind.
    fn take(&mut self) -> Handler {
        std::mem::take(self)
    }

    /// Run it, if it is there.
    ///
    /// # Safety
    ///
    /// The block's `invoke` is callable from this thread - for guest code, that
    /// means its code pointers are already thunks.
    unsafe fn run(&self, context: u64) {
        if self.block != 0 {
            // SAFETY: the caller's guarantee; `invoke` is at a fixed offset by the
            // Block ABI.
            unsafe { block::invoke(self.block) };
        } else if self.function != 0 {
            // `dispatch_source_set_event_handler_f` documents this: the handler is
            // called with the source's context, which is what
            // `dispatch_set_context` put there.
            // SAFETY: a `dispatch_function_t` the guest installed.
            unsafe { std::mem::transmute::<u64, Function>(self.function)(context as *mut c_void) };
        }
    }
}

/// The source a guest's handle names, if it names a live one.
///
/// # Safety
///
/// `handle` is null or an object this module allocated.
unsafe fn source<'a>(handle: u64) -> Option<&'a Source> {
    // SAFETY: the caller's guarantee, and the kind check makes the cast sound.
    unsafe { object::typed(handle, Kind::Source) }
        // SAFETY: a `Header` of kind `Source` is the first field of a `Source`.
        .map(|h| unsafe { &*(h as *const Header as *const Source) })
}

// ---------------------------------------------------------------------- create

/// `dispatch_source_t dispatch_source_create(dispatch_source_type_t, uintptr_t,
/// uintptr_t, dispatch_queue_t)`.
///
/// The second argument is the port and the third is the mask; see the module note
/// for why, and for the machine code that agrees.
pub unsafe extern "C" fn create(kind: u64, handle: u64, _mask: u64, queue: u64) -> u64 {
    match token_for(kind) {
        // A type this file has and can build a source for.
        Some(token) if token.implemented => {}
        _ => return refuse_type(kind),
    }
    // A `mach_port_t` is a `natural_t`, so Darwin keeps the low half of a
    // `uintptr_t` and so does this. Both call sites zero-extend a 32-bit load, so
    // nothing is lost.
    let port = handle as Name;
    if space::receiver(port).is_none() && space::members(port).is_none() {
        return refuse_port(port);
    }
    // Darwin substitutes the current queue for a null one, and the current queue
    // of a caller outside any queue is the default global queue. This module has
    // no notion of a current queue to substitute, so the default global queue is
    // what it substitutes - said here rather than left to be discovered.
    let queue = match queue {
        0 => {
            // SAFETY: no arguments that are not constants.
            unsafe { queue::get_global_queue(0, 0) }
        }
        q => q,
    };
    object::retain(queue);
    Box::into_raw(Box::new(Source {
        header: Header::new(Kind::Source),
        filter: Filter::MachRecv,
        port,
        queue,
        state: Mutex::new(State {
            event: Handler::default(),
            cancel: Handler::default(),
            cancelled: false,
            // Inactive until `dispatch_resume`, exactly as libdispatch's is.
            suspends: 1,
            firing: false,
        }),
    })) as u64
}

/// The `NULL` and the sentence for a source type this runtime does not implement.
fn refuse_type(kind: u64) -> u64 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: dispatch_source_create: refusing {}, token {kind:#x}: only \
             DISPATCH_SOURCE_TYPE_MACH_RECV is implemented, because a Mach receive right is a \
             descriptor this host can wait on and no other source type's condition is anything \
             here can observe. A null source is what Darwin answers for a type it does not \
             recognise; one that never fires would be the worse answer",
            type_name(kind),
        );
    });
    0
}

/// The `NULL` and the sentence for a name that is not a receive right here.
fn refuse_port(port: Name) -> u64 {
    eprintln!(
        "rosetta3: dispatch_source_create: refusing mach port {port:#x}: it is not a receive right \
         or a port set in this task's space, so no message can ever arrive for it. A source over a \
         name that does not resolve would never fire, and a watcher waiting on one is a hang \
         rather than a failure",
    );
    0
}

// -------------------------------------------------------------------- handlers

/// Which of a source's two handlers a call is about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Event,
    Cancel,
}

impl Which {
    /// Install `handler`, returning whatever was there.
    fn set(self, state: &mut State, handler: Handler) -> Handler {
        match self {
            Which::Event => std::mem::replace(&mut state.event, handler),
            Which::Cancel => std::mem::replace(&mut state.cancel, handler),
        }
    }

    fn block_name(self) -> &'static str {
        match self {
            Which::Event => "dispatch_source_set_event_handler",
            Which::Cancel => "dispatch_source_set_cancel_handler",
        }
    }
}

/// `void dispatch_source_set_event_handler(dispatch_source_t, dispatch_block_t)`.
///
/// Copied, because the handler outlives the call that installs it: the block the
/// guest passes is a stack literal whose captures live in the caller's frame, and
/// the module note quotes the `flags` that say so.
pub unsafe extern "C" fn set_event_handler(handle: u64, handler: u64) {
    // SAFETY: a live source, or a null handle, by the caller's contract.
    unsafe { set_block_handler(handle, handler, Which::Event) };
}

/// `void dispatch_source_set_cancel_handler(dispatch_source_t, dispatch_block_t)`.
pub unsafe extern "C" fn set_cancel_handler(handle: u64, handler: u64) {
    // SAFETY: a live source, or a null handle, by the caller's contract.
    unsafe { set_block_handler(handle, handler, Which::Cancel) };
}

/// Install a block as one of the source's two handlers.
unsafe fn set_block_handler(handle: u64, handler: u64, which: Which) {
    // SAFETY: a live source, or a null handle this returns from.
    let Some(s) = (unsafe { source(handle) }) else {
        return;
    };
    // A null block clears the handler, which is a documented spelling and not a
    // caller bug: `_Block_copy(NULL)` is NULL, and there is nothing to copy.
    let copied = block::copy(handler);
    if copied.is_none() && handler != 0 {
        announce_no_block_runtime(which.block_name());
        return;
    }
    // SAFETY: `s` is live, as above.
    let previous = which.set(&mut s.state.lock().unwrap_or_else(|e| e.into_inner()), Handler {
        block: copied.unwrap_or(0),
        function: 0,
    });
    // A handler replaced before it ever ran is still this module's reference.
    block::release(previous.block);
}

/// `void dispatch_source_set_event_handler_f(dispatch_source_t,
/// dispatch_function_t)`.
pub unsafe extern "C" fn set_event_handler_f(handle: u64, function: u64) {
    // SAFETY: a live source, or a null handle, by the caller's contract.
    unsafe { set_function_handler(handle, function, Which::Event) };
}

/// `void dispatch_source_set_cancel_handler_f(dispatch_source_t,
/// dispatch_function_t)`.
pub unsafe extern "C" fn set_cancel_handler_f(handle: u64, function: u64) {
    // SAFETY: a live source, or a null handle, by the caller's contract.
    unsafe { set_function_handler(handle, function, Which::Cancel) };
}

/// Install a `dispatch_function_t` as one of the source's two handlers.
///
/// No reference is taken and none is dropped, because there is nothing to own: a
/// function pointer, and the source's own context as its argument, is the whole
/// of what this spelling is.
unsafe fn set_function_handler(handle: u64, function: u64, which: Which) {
    // SAFETY: a live source, or a null handle this returns from.
    let Some(s) = (unsafe { source(handle) }) else {
        return;
    };
    // SAFETY: `s` is live, as above.
    let previous = which.set(
        &mut s.state.lock().unwrap_or_else(|e| e.into_inner()),
        Handler {
            block: 0,
            function,
        },
    );
    block::release(previous.block);
}

/// Say once that a block-taking call had no block runtime to copy with.
fn announce_no_block_runtime(name: &str) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: {name}: refused: no block runtime is registered, so a handler cannot be \
             copied off the caller's stack. The block is not queued un-copied: it points into a \
             frame that is about to return, and a handler that runs out of one is a crash \
             somewhere else"
        );
    });
}

// ------------------------------------------------------------ resume / suspend

/// `void dispatch_resume(dispatch_object_t)`.
///
/// The only object this means anything for is a source: a queue from
/// `dispatch_queue_create` is not suspended, and resuming one is what Darwin logs
/// as "Resume of an active object" and does nothing about. The guest resumes
/// queues too, so the distinction has to be made here rather than assumed.
pub unsafe extern "C" fn resume(handle: u64) {
    // SAFETY: a live object of this module's, or a null handle.
    let Some(s) = (unsafe { typed_source(handle) }) else {
        return;
    };
    let arm_now = {
        let mut state = s.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.suspends == 0 {
            return;
        }
        state.suspends -= 1;
        state.suspends == 0 && !state.cancelled && state.event != Handler::default()
    };
    if arm_now {
        arm(handle);
    }
}

/// `void dispatch_suspend(dispatch_object_t)`.
pub unsafe extern "C" fn suspend(handle: u64) {
    // SAFETY: a live object of this module's, or a null handle.
    let Some(s) = (unsafe { typed_source(handle) }) else {
        return;
    };
    {
        let mut state = s.state.lock().unwrap_or_else(|e| e.into_inner());
        // A count no `dispatch_resume` could bring back to zero is a source that
        // can never fire, so it is refused rather than counted.
        if state.suspends == u32::MAX {
            announce_too_many_suspends();
            return;
        }
        state.suspends += 1;
    }
    disarm(handle);
}

/// The `Source` a handle names, and only if it names a source rather than any
/// other dispatch object.
///
/// # Safety
///
/// `handle` is null or an object this module allocated.
unsafe fn typed_source<'a>(handle: u64) -> Option<&'a Source> {
    // SAFETY: the caller's guarantee, and the kind check makes the cast sound.
    if unsafe { object::typed(handle, Kind::Source) }.is_none() {
        return None;
    }
    // SAFETY: the kind check above is what makes this the same object.
    unsafe { source(handle) }
}

fn announce_too_many_suspends() {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: dispatch_suspend: refused: the source is already suspended as many times \
             as the count can hold, so the resume that would match this one is not coming"
        );
    });
}

/// Tear a source down at its last reference, and free it.
///
/// Called from [`super::object::release`] rather than doing the work there,
/// because the order is the whole of it and this is where the order lives:
///
/// 1. out of the watcher's `poll` set, so no descriptor this source was using can
///    be closed while a `poll` has it;
/// 2. the handlers released, so a guest block's captures go while the object they
///    were reached through is still whole;
/// 3. the target queue's reference dropped, which is this module's and the last
///    one to go;
/// 4. the allocation, last.
///
/// # Safety
///
/// `handle` is a source whose reference count has just reached zero, so no
/// delivery is outstanding - a delivery holds its own reference for its whole run,
/// which is what makes that true.
///
/// What makes the *watcher* safe is the `disarm` on the first line, and the
/// reason is in [`watcher`]: a `poll` snapshot taken before this call can name
/// this handle, and the liveness check that snapshot goes through holds the
/// registry's lock, which `disarm` also takes. So a snapshot entry that still
/// looks live is a source whose `disarm` has not run, and `disarm` is the first
/// thing here.
pub(crate) unsafe fn dispose(handle: u64) {
    // SAFETY: the caller's guarantee, and the kind check makes the cast sound.
    let Some(s) = (unsafe { source(handle) }) else {
        return;
    };
    // Before anything else: the watcher holds a `Vec<Arc<Receiver>>` per armed
    // source, and this is the only place that can say the source is gone.
    disarm(handle);
    let (event, cancel, queue) = {
        let mut state = s.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.event.take(), state.cancel.take(), s.queue)
    };
    // SAFETY: the guest's own blocks, released without a lock held because
    // `_Block_release` runs the guest's dispose helper.
    block::release(event.block);
    block::release(cancel.block);
    // SAFETY: the queue is live - this source holds a reference to it and the
    // finalizer has already run, so nothing has taken it.
    unsafe { object::release(queue) };
    // SAFETY: the refcount reached zero, so this reference is the only one, and
    // the type is named by the `Kind::Source` this was reached through.
    drop(unsafe { Box::from_raw(handle as *mut Source) });
}

// ---------------------------------------------------------------------- cancel

/// `void dispatch_source_cancel(dispatch_source_t)`.
///
/// Idempotent, and the handler runs exactly once: the second call is what Chromium
/// makes from `StopWatching` after the event handler has already cancelled the
/// same source, and libdispatch returns early when `DSF_CANCELED` is already set
/// rather than calling out twice.
///
/// A source that was never resumed still runs its cancel handler - that is
/// `_dispatch_source_activate`'s `DSF_CANCELED` path rather than a special case,
/// and the handler is submitted from here either way.
pub unsafe extern "C" fn cancel(handle: u64) {
    // SAFETY: a live source, or a null handle.
    let Some(s) = (unsafe { source(handle) }) else {
        return;
    };
    let (cancel, event, target) = {
        let mut state = s.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.cancelled {
            return;
        }
        state.cancelled = true;
        state.suspends = 0;
        // `_dispatch_source_cancel_callout` clears the pending data and frees the
        // event and registration handlers before it calls out.
        let cancel = state.cancel.take();
        let event = state.event.take();
        (cancel, event, s.queue)
    };
    // Outside the lock: releasing a block can run the guest's dispose helper, and
    // disarming wakes the watcher thread, which is not this one.
    block::release(event.block);
    disarm(handle);
    if cancel.block != 0 {
        // On the target queue, as Darwin runs it, and the copy comes **before**
        // this module's release: `_Block_copy` is what takes the reference the
        // queue then owns, and releasing first would free a block the copy is
        // about to read.
        queue::async_block(target, cancel.block);
        block::release(cancel.block);
        return;
    }
    if cancel.function != 0 {
        // SAFETY: a `dispatch_function_t` the guest installed, and the context it
        // is documented to be called with.
        let context = unsafe { object::get_context(handle) } as u64;
        // SAFETY: the function is the guest's own and the context is its own.
        unsafe { queue::async_f(target, context, cancel.function) };
    }
}

// -------------------------------------------------------------------- delivery

/// What a queued delivery carries, so it can run without the source's lock.
struct Delivery {
    source: u64,
    /// This delivery's own reference to the event handler. Taken when the delivery
    /// was queued rather than when it runs, so a `dispatch_source_cancel` on
    /// another thread releasing the source's cannot pull the block out from under
    /// the call.
    handler: Handler,
}

/// A `dispatch_function_t` for [`queue::async_f`], because the work has to do
/// something after the handler returns and `Work::Owned` has no room for that.
unsafe extern "C" fn run_delivery(context: *mut c_void) {
    // SAFETY: the box `fire` leaked, handed back here exactly once.
    let delivery = unsafe { Box::from_raw(context as *mut Delivery) };
    // SAFETY: the source is live: `fire` took this reference for the delivery.
    if let Some(s) = unsafe { source(delivery.source) } {
        // The handler runs with no lock held, because it is guest code and may
        // cancel this very source - which is what Chromium's one-shot watcher
        // does - and may submit to the same queue.
        //
        // SAFETY: a handler this delivery owns, and a function pointer the guest
        // installed.
        unsafe { delivery.handler.run(object::get_context(delivery.source) as u64) };
        // The source is re-armed by **clearing `firing` and nothing else**, and
        // that is the whole of the level-triggered mechanism: an armed source
        // stays in the watcher's `poll` set for its whole life, so the next `poll`
        // is what decides whether it fires again. Calling [`fire`] from here
        // instead would queue a delivery per handler run with no readiness test in
        // between - which, for a handler that takes the message off the port, is a
        // spin with nothing left to deliver. That was the bug this comment
        // replaces.
        s.state.lock().unwrap_or_else(|e| e.into_inner()).firing = false;
    }
    // The source may have gone away between the queue taking this work and running
    // it, which the guest's own release caused and which the reference `fire` took
    // is there to prevent; the `if let` is for a null-kind handle rather than for
    // that.
    block::release(delivery.handler.block);
    // SAFETY: the reference `fire` took for this delivery.
    unsafe { object::release(delivery.source) };
}

/// Submit `handle`'s event handler to its target queue, once per readiness.
fn fire(handle: u64) {
    // SAFETY: the caller holds a reference, or the source is this module's own.
    let Some(s) = (unsafe { source(handle) }) else {
        return;
    };
    let (target, handler) = {
        let mut state = s.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.cancelled || state.suspends > 0 || state.firing {
            return;
        }
        if state.event == Handler::default() {
            return;
        }
        // Copied under the lock, so a `dispatch_source_cancel` on another thread
        // cannot release the source's only reference between the test above and
        // the copy.
        let handler = state.event;
        let taken = match handler.block {
            0 => handler,
            block => match block::copy(block) {
                Some(copied) => Handler {
                    block: copied,
                    function: 0,
                },
                None => {
                    announce_no_block_runtime("a dispatch source event handler");
                    return;
                }
            },
        };
        state.firing = true;
        (s.queue, taken)
    };
    // The queue holds the source for as long as the delivery is outstanding.
    object::retain(handle);
    let boxed = Box::into_raw(Box::new(Delivery {
        source: handle,
        handler,
    })) as u64;
    // SAFETY: the function is this module's and the context is the box above.
    unsafe { queue::async_f(target, boxed, run_delivery as *const () as usize as u64) };
}

// -------------------------------------------------------------------- watching

/// The descriptors a source over `name` waits on, or `None` if `name` is not a
/// receive right or a port set in this task's space.
///
/// The `Arc`s are the point of the return: the receive right has to be held for
/// as long as its descriptor is in somebody's `poll` set, and the only handle
/// this crate has on one is the port space's.
///
/// Six lines of [`crate::mach::kevent64`]'s `ports_behind`, repeated rather than
/// shared: that function is private to a module this one must not edit, and two
/// callers reading the same rule is cheaper than one reaching across a boundary to
/// ask for it.
fn ports_behind(name: Name) -> Option<Vec<Arc<Receiver>>> {
    if let Some(port) = space::receiver(name) {
        return Some(vec![port]);
    }
    // A set with no usable member is a set whose event can never fire, which is
    // what Darwin does too, so it registers - as an empty set.
    Some(
        space::members(name)?
            .into_iter()
            .filter_map(space::receiver)
            .collect(),
    )
}

/// One armed source: the handle to fire, and the receive rights its descriptor
/// list is made of. Holding the `Arc`s is what keeps those descriptors open
/// across the `poll`.
#[derive(Clone)]
struct Armed {
    source: u64,
    ports: Vec<Arc<Receiver>>,
}

struct Registry {
    armed: Vec<Armed>,
    /// The self-pipe: the read end is entry zero of the watcher's `poll` set, so a
    /// change to `armed` is told apart from a source firing without counting the
    /// sources.
    read: i32,
    write: i32,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let (read, write) = pipe().unwrap_or_else(|error| {
            // Without a pipe the watcher could not be woken for a source armed or
            // cancelled while it is blocked, which is the normal case. Refusing to
            // arm is better than arming into a wait nothing interrupts.
            panic!("rosetta3: dispatch source: no self-pipe for the watcher: {error}");
        });
        Mutex::new(Registry {
            armed: Vec::new(),
            read,
            write,
        })
    })
}

/// A pipe, read end first, both ends non-blocking.
fn pipe() -> std::io::Result<(i32, i32)> {
    let mut fds = [0 as i32; 2];
    // SAFETY: two writable `int`s, and the count matches.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: `fd` is one of the two just opened and still owned here.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    Ok((fds[0], fds[1]))
}

/// Put `handle` in the watcher's `poll` set.
fn arm(handle: u64) {
    // SAFETY: a live source, or a handle of this module's.
    let Some(s) = (unsafe { source(handle) }) else {
        return;
    };
    let Some(ports) = ports_behind(s.port) else {
        return;
    };
    if ports.is_empty() {
        announce_vanished(s.port);
        return;
    }
    {
        let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        if registry.armed.iter().any(|a| a.source == handle) {
            return;
        }
        registry.armed.push(Armed { source: handle, ports });
    }
    start_watcher();
    wake();
}

/// Take `handle` out of the watcher's `poll` set.
fn disarm(handle: u64) {
    let changed = {
        let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        let before = registry.armed.len();
        registry.armed.retain(|a| a.source != handle);
        registry.armed.len() != before
    };
    if changed {
        wake();
    }
}

/// A port's receive right is gone and is not coming back, said once.
fn announce_vanished(port: Name) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: dispatch source: mach port {port:#x} is no longer a receive right or a \
             non-empty port set, so the source watching it is not registered and will not fire. \
             Mach reports the knote as vanished and stops; the source is left alone rather than \
             reported as ready, because a watcher that is never told is a hang"
        );
    });
}

/// Wake the watcher: one byte, and the reason the pipe exists.
fn wake() {
    let byte = [0u8; 1];
    // SAFETY: one readable byte, and the length matches.
    unsafe {
        libc::write(
            registry().lock().unwrap_or_else(|e| e.into_inner()).write,
            byte.as_ptr().cast(),
            1,
        );
    }
}

/// Start the one watcher thread, if it is not running.
fn start_watcher() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        // A failure here leaves the sources armed and unpolled, which is a hang
        // rather than a crash, so it is said rather than swallowed.
        if std::thread::Builder::new()
            .name("dispatch-source".into())
            .stack_size(256 * 1024)
            .spawn(watcher)
            .is_err()
        {
            announce_no_watcher();
        }
    });
}

fn announce_no_watcher() {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: dispatch source: the watcher thread did not start, so a Mach receive \
             source will never fire. A waitable event's watcher is one of these, and it would \
             hang the event loop rather than fail"
        );
    });
}

/// Block on every armed source's descriptor until one of them is readable.
///
/// A snapshot rather than a lock held across the `poll`, because the `poll` blocks
/// and an arm on another thread has to be able to take the registry to write the
/// pipe that interrupts it. The snapshot is only as stale as one pass of this loop,
/// and any change to it writes the pipe, so a change made during a `poll` brings
/// this one back round.
fn watcher() {
    let read = registry().lock().unwrap_or_else(|e| e.into_inner()).read;
    loop {
        let armed: Vec<Armed> = registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .armed
            .clone();
        let mut entries: Vec<libc::pollfd> = Vec::with_capacity(1 + armed.len());
        entries.push(libc::pollfd {
            fd: read,
            events: libc::POLLIN,
            revents: 0,
        });
        for entry in &armed {
            for port in &entry.ports {
                entries.push(libc::pollfd {
                    fd: port.fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
        }
        // SAFETY: `entries` is the number of live `struct pollfd` built above, and
        // the descriptors in it are held by the `Armed` clones in `armed`.
        let ready = unsafe {
            libc::poll(
                entries.as_mut_ptr(),
                entries.len() as libc::nfds_t,
                -1, // no timeout: an untimed wait, which is what an event source is
            )
        };
        if ready < 0 {
            // `EINTR` is not a failure; the loop takes a new snapshot.
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                announce_poll_failed();
                return;
            }
            continue;
        }
        if entries[0].revents != 0 {
            drain(read);
            continue;
        }
        // `entries[1..]` is `armed`'s descriptors in the order they were pushed,
        // which is what makes this a walk of both at once.
        let mut at = 1;
        let mut ready: Vec<u64> = Vec::new();
        for entry in &armed {
            let fired = entry.ports.iter().zip(&entries[at..]).any(|(port, e)| {
                e.fd == port.fd()
                    && e.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL)
                        != 0
            });
            at += entry.ports.len();
            if fired {
                ready.push(entry.source);
            }
        }
        // **The liveness check happens here, under the registry's own lock**, and
        // that is load-bearing rather than tidiness. `armed` is a snapshot taken
        // before the `poll`, and a source in it can be released - and its memory
        // freed by [`super::object::release`] - while this thread is blocked. The
        // only thing that frees one is [`dispose`], and the first thing `dispose`
        // does is take this lock and remove the entry, so:
        //
        // * still in the table while this lock is held means the object has not
        //   been freed, and cannot be until this lock is dropped;
        // * a `dispose` already past its `disarm` is blocked here, so its
        //   `Box::from_raw` has not run either.
        //
        // A lock-ordering note, since this is the one place two of this module's
        // locks are held at once: the order is registry then source state, and
        // nothing takes them the other way. `resume`, `suspend`, `cancel` and
        // `dispose` all drop the source's state lock before they touch the
        // registry, and `fire` takes the state lock and drops it before it
        // queues anything.
        {
            let registry = registry().lock().unwrap_or_else(|e| e.into_inner());
            for source in ready {
                if registry.armed.iter().any(|a| a.source == source) {
                    fire(source);
                }
            }
        }
    }
}

fn announce_poll_failed() {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: dispatch source: the watcher's poll failed and the thread has stopped, so \
             a Mach receive source will never fire"
        );
    });
}

/// Read what the pipe has, so the next `poll` is not woken by the same byte.
fn drain(read: i32) {
    let mut bytes = [0u8; 64];
    loop {
        // SAFETY: `bytes` is 64 writable bytes and the length matches.
        let read_bytes = unsafe { libc::read(read, bytes.as_mut_ptr().cast(), bytes.len()) };
        if read_bytes < bytes.len() as isize {
            return;
        }
    }
}

#[cfg(test)]
#[allow(non_snake_case)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    /// A block literal, so the invoke path is the real one rather than a stand-in.
    /// `flags` is zero, which says there is no copy helper: these literals are
    /// already on the heap and the test's block runtime hands the same pointer
    /// back.
    ///
    /// The captures follow the literal, which is where the Block ABI puts them and
    /// where `block::invoke` does not care.
    #[repr(C)]
    struct Literal {
        isa: u64,
        flags: u32,
        reserved: u32,
        invoke: u64,
        descriptor: u64,
        counter: u64,
        extra: u64,
    }

    /// Byte offset of the first capture.
    const CAPTURE: u64 = std::mem::offset_of!(Literal, counter) as u64;

    /// The `pthread_self()` the last handler saw, for the test that cares which
    /// thread it was.
    static HANDLER_THREAD: AtomicU64 = AtomicU64::new(0);

    /// The receive right each test's handler drains, keyed by that test's counter.
    ///
    /// **Every handler here drains, and that is load-bearing rather than tidy.**
    /// A `DISPATCH_SOURCE_TYPE_MACH_RECV` event is level-triggered - the message
    /// is still at the head of the port until somebody receives it - so a handler
    /// that neither drains nor cancels has its source fire again on the next
    /// `poll`, for as long as the source lives. One such source in this module
    /// would leave the shared watcher and the shared worker pool spinning for the
    /// rest of the run and make every other test's timing a function of how many
    /// tests run in parallel; the suite flakes about one run in three without it.
    ///
    /// Keyed by the counter so two tests cannot drain each other's port, and so
    /// [`armed_source`] can register the port for a caller that only has a source.
    static PORTS: std::sync::LazyLock<Mutex<HashMap<u64, Arc<Receiver>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

    /// Record which thread a handler ran on.
    fn note_thread() {
        HANDLER_THREAD.store(crate::pthread::id() as u64, Ordering::Release);
    }

    /// Take the message off the port registered for `counter`, if there is one.
    fn drain_for(counter: &AtomicU32) {
        let right = PORTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(counter as *const AtomicU32 as u64))
            .cloned();
        if let Some(right) = right {
            let _ = right.receive(0);
        }
    }

    /// The body every generated `invoke` shares: count, then drain.
    fn count_capture(block: u64, counter: &AtomicU32) {
        note_thread();
        // The capture holds the *address* of the counter, so reaching it is two
        // steps, and one of them being easy to forget is why this is written out
        // once here rather than in each `invoke`.
        //
        // SAFETY: a literal this module built, whose capture at `CAPTURE` is the
        // address of a static, which outlives every call here.
        let named = unsafe { *((block + CAPTURE) as *const u64) };
        assert_eq!(named, counter as *const AtomicU32 as u64, "capture");
        counter.fetch_add(1, Ordering::AcqRel);
        drain_for(counter);
    }

    /// One counter per test, and one `invoke` per counter. A single counter shared
    /// across cases that run in parallel is a test that passes for the wrong reason.
    macro_rules! counter {
        ($name:ident) => {
            static $name: AtomicU32 = AtomicU32::new(0);
        };
    }

    counter!(DELIVERED);
    counter!(NOT_RESUMED);
    counter!(CANCELLED);
    counter!(UNRESUMED_CANCEL);
    counter!(QUEUE_RESUME);
    counter!(RELEASED);
    counter!(LEVEL);
    counter!(ONE_SHOT);

    /// Cancels the source the block belongs to, which is what Chromium's watcher
    /// does and is why a `dispatch_source_cancel` from inside a handler has to be
    /// safe. The handle is the second capture, after the counter.
    extern "C" fn one_shot_invoke(block: u64) {
        count_capture(block, &ONE_SHOT);
        // SAFETY: a literal this module built, whose second capture is the source
        // its own test created and has not released.
        let source = unsafe { *((block + CAPTURE + 8) as *const u64) };
        // SAFETY: a source this test created and has not released.
        unsafe { cancel(source) };
    }

    static CONTEXT: AtomicU32 = AtomicU32::new(0);

    /// The `dispatch_function_t` spelling, which takes the source's context rather
    /// than a capture.
    unsafe extern "C" fn take_context(context: *mut c_void) {
        CONTEXT.store(context as usize as u32, Ordering::Release);
    }

    /// A block runtime, because the block-taking entry points refuse without one -
    /// which is the behaviour this module wants, and which would otherwise leave
    /// the delivery path untested.
    fn test_block_runtime() {
        unsafe extern "C" fn copy(block: u64) -> u64 {
            block
        }
        unsafe extern "C" fn release(_block: u64) {}
        block::set_runtime(
            copy as *const () as usize as u64,
            release as *const () as usize as u64,
        );
    }

    /// A block literal whose captures are `counter` and `extra`.
    fn literal(counter: &AtomicU32, extra: u64, invoke: extern "C" fn(u64)) -> u64 {
        assert_eq!(CAPTURE, 32, "the captures follow the literal");
        assert_eq!(
            std::mem::offset_of!(Literal, invoke) as u64,
            16,
            "the Block ABI puts invoke at offset 16"
        );
        Box::into_raw(Box::new(Literal {
            isa: 0,
            flags: 0,
            reserved: 0,
            invoke: invoke as *const () as usize as u64,
            descriptor: 0,
            counter: counter as *const AtomicU32 as u64,
            extra,
        })) as u64
    }

    /// The common shape: a source over a fresh receive right, armed, with the
    /// handler draining that port so nothing is left spinning.
    ///
    /// Returns the source, the address to send to, and the receive right.
    fn armed_source(counter: &AtomicU32) -> (u64, u64, Arc<Receiver>) {
        let right = Arc::new(Receiver::bind().expect("bind"));
        PORTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(counter as *const AtomicU32 as u64, right.clone());
        let (source, address) = source_over(&right);
        // SAFETY: a live source, and a literal this test owns.
        unsafe {
            set_event_handler(source, literal(counter, 0, invoke_of(counter)));
            resume(source);
        }
        (source, address, right)
    }

    /// The `invoke` that belongs to `counter`'s generated literal.
    ///
    /// Every test's handler is the same shape - count, then drain - so one of them
    /// serves them all and the counter is what tells them apart.
    fn invoke_of(_counter: &AtomicU32) -> extern "C" fn(u64) {
        // Chosen by the address the literal's capture names, so a test cannot end
        // up counting into a counter its assertions are not reading.
        counting_invoke
    }

    extern "C" fn counting_invoke(block: u64) {
        // SAFETY: a literal this module built, whose capture at `CAPTURE` is the
        // address of a static, which outlives every call here.
        let counter = unsafe { *((block + CAPTURE) as *const u64) };
        // SAFETY: the capture is a static's address, by construction above.
        count_capture(block, unsafe { &*(counter as *const AtomicU32) });
    }

    /// A source over a fresh receive right, which is the shape both of the guest's
    /// Mach receive call sites have. Nothing registered, so a handler that drains
    /// drains nothing - which is what the level-triggered test needs.
    fn source_over(right: &Arc<Receiver>) -> (u64, u64) {
        let port = space::insert(space::Right::Receive(right.clone()));
        let address = space::address_of(port).expect("a receive right has an address");
        // SAFETY: a name this test just allocated, and a null queue.
        let source = unsafe { create(mach_recv_token(), port as u64, 0, 0) };
        assert_ne!(source, 0, "a live receive right is a source");
        (source, address)
    }

    /// A source over a fresh receive right, with no handler and nothing armed.
    fn bare_source() -> (u64, u64, Arc<Receiver>) {
        let right = Arc::new(Receiver::bind().expect("bind"));
        let (source, address) = source_over(&right);
        (source, address, right)
    }

    /// A port with something on it, so a source watching it has a reason to fire.
    fn send_to(address: u64, bytes: &'static [u8]) {
        crate::mach::wire::send(address, bytes, &[]).expect("send");
    }

    fn wait_for(counter: &AtomicU32) -> bool {
        for _ in 0..300 {
            if counter.load(Ordering::Acquire) > 0 {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    /// Whether `counter` stops growing within a window.
    ///
    /// **Not** "the count is exactly N", because `poll` does not promise that. A
    /// `poll` that reports a descriptor ready can be reporting a datagram that
    /// another thread took between the readiness check and the return - the
    /// descriptor's lock is not held across both - so a source can be delivered
    /// to once more than there were messages. Every poll-driven loop tolerates
    /// that; `kqueue` has the same property. The claim worth asserting is that the
    /// count *stops*, which is what a spin would break and a bounded extra
    /// delivery does not.
    fn stops_growing(counter: &AtomicU32) -> u32 {
        let first = counter.load(Ordering::Acquire);
        std::thread::sleep(std::time::Duration::from_millis(100));
        let second = counter.load(Ordering::Acquire);
        assert_eq!(
            first, second,
            "the source is still being delivered to: {} then {}",
            first, second
        );
        second
    }

    /// The whole point: a message sent to the port the source watches reaches the
    /// guest's block, on a thread that is not the one that sent it.
    #[test]
    fn a_message_reaches_the_handlers_block_on_another_thread() {
        test_block_runtime();
        DELIVERED.store(0, Ordering::Release);
        let (source, address, _) = armed_source(&DELIVERED);
        send_to(address, b"wake up");
        assert!(wait_for(&DELIVERED), "the handler never ran");
        assert_ne!(
            HANDLER_THREAD.load(Ordering::Acquire),
            crate::pthread::id() as u64,
            "the handler ran on the thread that sent the message"
        );
        // SAFETY: the guest is giving up its reference.
        unsafe {
            cancel(source);
            object::release(source);
        }
    }

    /// A source that has not been resumed must not fire, and must fire once it is.
    #[test]
    fn nothing_is_delivered_before_the_resume() {
        test_block_runtime();
        NOT_RESUMED.store(0, Ordering::Release);
        let (source, address, _) = bare_source();
        // SAFETY: a live source, and a literal this test owns. No `resume` yet.
        unsafe { set_event_handler(source, literal(&NOT_RESUMED, 0, counting_invoke)) };
        send_to(address, b"too early");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(
            NOT_RESUMED.load(Ordering::Acquire),
            0,
            "an inactive source delivered an event"
        );
        // SAFETY: a live source.
        unsafe { resume(source) };
        assert!(wait_for(&NOT_RESUMED), "the resume did not arm the source");
        // SAFETY: the guest is giving up its reference.
        unsafe {
            cancel(source);
            object::release(source);
        }
    }

    /// `dispatch_source_cancel` twice is what Chromium does - once from the
    /// handler, once from `StopWatching` - and the cancel handler runs once.
    #[test]
    fn a_second_cancel_runs_nothing_a_second_time() {
        test_block_runtime();
        CANCELLED.store(0, Ordering::Release);
        let (source, _, _) = bare_source();
        // SAFETY: a live source, and a literal this test owns.
        unsafe {
            set_cancel_handler(source, literal(&CANCELLED, 0, counting_invoke));
            cancel(source);
            cancel(source);
            object::release(source);
        }
        assert!(wait_for(&CANCELLED), "the cancel handler never ran");
        assert_eq!(CANCELLED.load(Ordering::Acquire), 1, "it ran twice");
    }

    /// The `base::apple` teardown at `+0x5d928b0` depends on this: a source that
    /// was never resumed still runs its cancel handler, and the guest then blocks
    /// on a semaphore only the handler can signal.
    #[test]
    fn a_source_that_was_never_resumed_still_cancels() {
        test_block_runtime();
        UNRESUMED_CANCEL.store(0, Ordering::Release);
        let (source, _, _) = bare_source();
        // SAFETY: a live source, and a literal this test owns. No `resume`.
        unsafe {
            set_cancel_handler(source, literal(&UNRESUMED_CANCEL, 0, counting_invoke));
            cancel(source);
            object::release(source);
        }
        assert!(wait_for(&UNRESUMED_CANCEL), "the cancel handler never ran");
    }

    /// The `_f` spelling is a function and the source's own context, and the guest
    /// installs one for `base::apple` - so the context has to arrive, and it has
    /// to arrive on the target queue rather than on the cancelling thread.
    #[test]
    fn the_function_spelling_is_called_with_the_sources_context() {
        CONTEXT.store(0, Ordering::Release);
        let (source, _, _) = bare_source();
        // SAFETY: a live source, and a function pointer of this module's own.
        unsafe {
            object::set_context(source, 0x5eed as *mut c_void);
            set_cancel_handler_f(source, take_context as *const () as usize as u64);
            cancel(source);
            object::release(source);
        }
        assert!(wait_for(&CONTEXT), "the `_f` handler never ran");
        assert_eq!(
            CONTEXT.load(Ordering::Acquire),
            0x5eed,
            "the handler was not called with the source's context"
        );
    }

    /// A handler that cancels its own source must not be queued again. Chromium's
    /// one-shot watcher does exactly this, and a source that kept firing would be a
    /// spin on a port whose message is still at the head of the queue.
    #[test]
    fn a_handler_that_cancels_its_own_source_is_not_fired_again() {
        test_block_runtime();
        ONE_SHOT.store(0, Ordering::Release);
        let right = Arc::new(Receiver::bind().expect("bind"));
        let (source, address) = source_over(&right);
        // SAFETY: a live source, and a literal this test owns.
        unsafe {
            set_event_handler(
                source,
                literal(&ONE_SHOT, source, one_shot_invoke),
            );
            resume(source);
        }
        // Three messages, and the handler takes none of them, so the port stays
        // readable and the source is ready again the moment the handler returns.
        for _ in 0..3 {
            send_to(address, b"wake up");
        }
        assert!(wait_for(&ONE_SHOT), "the handler never ran");
        // A cancelled source is out of the watcher's set, so the count has to stop
        // rather than reach a particular number - see [`stops_growing`].
        assert!(
            stops_growing(&ONE_SHOT) <= 2,
            "a cancelled source was delivered to {} times",
            ONE_SHOT.load(Ordering::Acquire)
        );
        // SAFETY: the guest is giving up its reference.
        unsafe { object::release(source) };
    }

    /// The last release of a source tears it down: out of the watcher's set, its
    /// handlers released, its queue reference dropped, its memory freed. The test
    /// holds its own reference so the teardown happens at a point it chooses -
    /// otherwise the last release lands wherever an in-flight delivery happens to
    /// put it, and a message sent in that window is a *legitimate* delivery, which
    /// would make the test fail for the wrong reason.
    #[test]
    fn the_last_release_tears_the_source_down() {
        test_block_runtime();
        RELEASED.store(0, Ordering::Release);
        let (source, address, _) = armed_source(&RELEASED);
        // The test's own reference, so the guest's release below is not the last
        // one and the timing is this test's rather than a worker's.
        object::retain(source);
        send_to(address, b"wake up");
        assert!(wait_for(&RELEASED), "the handler never ran");
        // The guest's reference, with no cancel first: the teardown has to disarm
        // and free on its own, and with this test holding one it does not happen
        // yet.
        //
        // SAFETY: giving up the reference `create` returned.
        unsafe { object::release(source) };
        // The last one, on this thread, with nothing in flight.
        //
        // SAFETY: giving up the reference taken above.
        unsafe { object::release(source) };
        // A message now has nothing to fire: the source is out of the watcher's
        // set, and its `Arc<Receiver>` with it.
        send_to(address, b"after the release");
        assert!(
            stops_growing(&RELEASED) <= 2,
            "a released source was still delivered to: {}",
            RELEASED.load(Ordering::Acquire)
        );
    }

    /// A handler that neither drains the port nor cancels is a spin, and that is
    /// Darwin's answer rather than this runtime's: the event is level-triggered
    /// because the message is still at the head of the queue. This test says so, so
    /// the behaviour is asserted rather than merely not contradicted - a silent
    /// spin would be the thing worth being surprised by.
    #[test]
    fn a_handler_that_neither_drains_nor_cancels_is_level_triggered() {
        test_block_runtime();
        LEVEL.store(0, Ordering::Release);
        // No port registered, so this handler's drain finds nothing: the point is
        // the source staying ready, not the drain.
        let right = Arc::new(Receiver::bind().expect("bind"));
        let (source, address) = source_over(&right);
        // SAFETY: a live source, and a literal this test owns.
        unsafe {
            set_event_handler(source, literal(&LEVEL, 0, never_drains));
            resume(source);
        }
        send_to(address, b"wake up");
        assert!(wait_for(&LEVEL), "the handler never ran");
        let mut fired_twice = false;
        for _ in 0..200 {
            if LEVEL.load(Ordering::Acquire) > 1 {
                fired_twice = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // SAFETY: the guest is giving up its references. Cancelling is what stops
        // the spin, and it is why this test cancels before it asserts rather than
        // after: a source left spinning outlives the test.
        unsafe {
            cancel(source);
            object::release(source);
        }
        assert!(
            fired_twice,
            "a level-triggered source stopped re-firing, so a handler that does not drain the \
             port would be delivered to once and never again"
        );
    }

    /// Counts and never drains, which is the one behaviour in this module that
    /// leaves its source level-triggered.
    extern "C" fn never_drains(block: u64) {
        note_thread();
        // SAFETY: a literal this module built, whose capture at `CAPTURE` is the
        // address of a static, which outlives every call here.
        let counter = unsafe { &*(*((block + CAPTURE) as *const u64) as *const AtomicU32) };
        counter.fetch_add(1, Ordering::AcqRel);
    }

    /// A type this runtime does not implement is a refusal, and the refusal is
    /// visible: a source that never fires is a hang, so it must not be one that
    /// looks like a source.
    #[test]
    fn another_source_type_is_refused_rather_than_answered() {
        // SAFETY: all four are addresses, and `create` reads nothing else.
        unsafe {
            assert_eq!(
                create(proc_token(), 0x1234, 0, 0),
                0,
                "a source type with no mechanism behind it must not become a source"
            );
            assert_eq!(create(memorypressure_token(), 0x1234, 0, 0), 0);
            assert_eq!(create(0, 0x1234, 0, 0), 0);
            // And a Mach receive right that is not in this task's space is refused
            // for the same reason.
            assert_eq!(create(mach_recv_token(), 0xdead_beef, 0, 0), 0);
        }
    }

    /// The type tokens have to work *as* tokens: no two compared equal, and none
    /// null.
    #[test]
    fn the_type_tokens_are_distinct_and_not_null() {
        let tokens = [mach_recv_token(), proc_token(), memorypressure_token()];
        for (i, token) in tokens.iter().enumerate() {
            assert_ne!(*token, 0, "a null source type is one a caller cannot compare");
            for other in &tokens[i + 1..] {
                assert_ne!(token, other, "two source types share an address");
            }
        }
    }

    /// `dispatch_resume` is called on queues too, and a queue is not a source, so
    /// resuming one must not be read as resuming a source and must not arm it.
    #[test]
    fn resuming_a_queue_does_not_touch_a_source() {
        test_block_runtime();
        QUEUE_RESUME.store(0, Ordering::Release);
        let (source, address, _) = bare_source();
        // SAFETY: a live source, and a literal this test owns. No `resume` yet.
        unsafe { set_event_handler(source, literal(&QUEUE_RESUME, 0, counting_invoke)) };
        let queue = queue::create_serial("resume test");
        // SAFETY: both are live objects of this directory.
        unsafe {
            resume(queue);
            suspend(queue);
            resume(queue);
            resume(source);
        }
        send_to(address, b"x");
        assert!(wait_for(&QUEUE_RESUME), "the source never fired after the queue's resume");
        // SAFETY: the guest is giving up its references.
        unsafe {
            cancel(source);
            object::release(source);
            object::release(queue);
        }
    }
}
