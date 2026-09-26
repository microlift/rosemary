#![cfg(not(target_os = "macos"))]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, RwLock};

use super::kern::*;
use super::wire::{Address, Receiver};

/// `mach_port_name_t`, and the two values that are not names.
pub type Name = u32;
pub const MACH_PORT_NULL: Name = 0;
pub const MACH_PORT_DEAD: Name = !0;

/// `MACH_PORT_RIGHT_*`, in the order `<mach/port.h>` declares them. Send is
/// zero, which is the one worth reading twice - `mach_port_allocate` is nearly
/// always called with `1`.
pub const RIGHT_SEND: u32 = 0;
pub const RIGHT_RECEIVE: u32 = 1;
pub const RIGHT_SEND_ONCE: u32 = 2;
pub const RIGHT_PORT_SET: u32 = 3;
pub const RIGHT_DEAD_NAME: u32 = 4;

/// `MACH_PORT_TYPE(right)`, which is `1 << (right + 16)`.
pub const fn type_of(right: u32) -> u32 {
    1 << (right + 16)
}

/// The names that exist before anything is allocated.
///
/// Fixed rather than drawn from the counter so that `mach_task_self_` can be a
/// constant: it is a *data* import, bound before a line of guest code runs, and
/// a data import that read zero is the segfault this whole directory exists to
/// stop.
pub const TASK_SELF: Name = 0x103;
pub const HOST_SELF: Name = 0x203;
pub const BOOTSTRAP: Name = 0x303;
/// The two clock services `host_get_clock_service` hands out. Fixed because
/// every caller asking for the same clock gets the same port on a real system.
pub const SYSTEM_CLOCK: Name = 0x403;
pub const CALENDAR_CLOCK: Name = 0x503;
/// The first name [`allocate`] hands out. Above the fixed ones, and stepped by
/// four the way a real space's names are, so a caller that assumes a name is
/// not a small integer is not surprised.
const FIRST_DYNAMIC: Name = 0x1003;

/// What a name refers to.
pub enum Right {
    /// The receiving end. Held by `Arc` because a port set names its members and
    /// a receive in progress must keep the socket alive.
    Receive(Arc<Receiver>),
    /// A send right: the address of somebody else's receive right, and the user
    /// reference count Mach makes the caller balance.
    Send { address: Address, refs: u32 },
    /// A send-once right, which is destroyed by the message that uses it.
    SendOnce { address: Address },
    /// A port set, which has no queue of its own - only members.
    Set(Vec<Name>),
    /// A right whose port has been destroyed.
    Dead { refs: u32 },
    /// A Mach semaphore, which is named out of the same space and is not a port.
    Semaphore(Arc<super::sync::Semaphore>),
    /// A name this runtime answers for without a port behind it: the task, the
    /// host, a thread, the bootstrap server, a clock.
    Special,
}

impl Right {
    fn type_bits(&self) -> u32 {
        match self {
            Right::Receive(_) => type_of(RIGHT_RECEIVE),
            Right::Send { .. } => type_of(RIGHT_SEND),
            Right::SendOnce { .. } => type_of(RIGHT_SEND_ONCE),
            Right::Set(_) => type_of(RIGHT_PORT_SET),
            Right::Dead { .. } => type_of(RIGHT_DEAD_NAME),
            // Neither is a port right. A caller asking after one is asking about
            // something this host does not model, and `mach_port_type` reports
            // it as a send right because that is what the task and host ports
            // are on a real system.
            Right::Semaphore(_) | Right::Special => type_of(RIGHT_SEND),
        }
    }
}

struct Space {
    names: HashMap<Name, Right>,
    next: Name,
}

fn space() -> &'static RwLock<Space> {
    static SPACE: std::sync::OnceLock<RwLock<Space>> = std::sync::OnceLock::new();
    SPACE.get_or_init(|| {
        let mut names = HashMap::new();
        names.insert(TASK_SELF, Right::Special);
        names.insert(HOST_SELF, Right::Special);
        names.insert(BOOTSTRAP, Right::Special);
        names.insert(SYSTEM_CLOCK, Right::Special);
        names.insert(CALENDAR_CLOCK, Right::Special);
        RwLock::new(Space {
            names,
            next: FIRST_DYNAMIC,
        })
    })
}

fn with<R>(f: impl FnOnce(&mut Space) -> R) -> R {
    let mut guard = space().write().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

/// Put `right` in the table under a fresh name.
pub fn insert(right: Right) -> Name {
    with(|space| {
        let name = space.next;
        space.next = space.next.wrapping_add(4);
        space.names.insert(name, right);
        name
    })
}

/// The address a send or send-once right names, and whether it was send-once.
///
/// A send-once right is *not* consumed here: a message that fails to send has
/// not used it, so [`super::msg`] consumes it only once the datagram is away.
pub fn destination(name: Name) -> Result<(Address, bool), i32> {
    let guard = space().read().unwrap_or_else(|e| e.into_inner());
    match guard.names.get(&name) {
        Some(Right::Send { address, .. }) => Ok((*address, false)),
        Some(Right::SendOnce { address }) => Ok((*address, true)),
        // A receive right is a legal destination: a task can send to itself.
        Some(Right::Receive(port)) => Ok((port.address(), false)),
        Some(Right::Dead { .. }) => Err(MACH_SEND_INVALID_DEST),
        Some(_) => Err(MACH_SEND_INVALID_RIGHT),
        None => Err(MACH_SEND_INVALID_DEST),
    }
}

/// The receive right `name` refers to.
pub fn receiver(name: Name) -> Option<Arc<Receiver>> {
    let guard = space().read().unwrap_or_else(|e| e.into_inner());
    match guard.names.get(&name) {
        Some(Right::Receive(port)) => Some(port.clone()),
        _ => None,
    }
}

/// Look at whatever `name` refers to, without copying it out.
pub fn with_right<R>(name: Name, f: impl FnOnce(&Right) -> R) -> Option<R> {
    let guard = space().read().unwrap_or_else(|e| e.into_inner());
    guard.names.get(&name).map(f)
}

/// The members of a port set, or `None` if `name` is not one.
pub fn members(name: Name) -> Option<Vec<Name>> {
    let guard = space().read().unwrap_or_else(|e| e.into_inner());
    match guard.names.get(&name) {
        Some(Right::Set(members)) => Some(members.clone()),
        _ => None,
    }
}

/// The address a right names, whatever kind it is - for a descriptor that has
/// to be encoded without regard to what the caller is doing with it.
pub fn address_of(name: Name) -> Option<Address> {
    let guard = space().read().unwrap_or_else(|e| e.into_inner());
    match guard.names.get(&name) {
        Some(Right::Receive(port)) => Some(port.address()),
        Some(Right::Send { address, .. }) | Some(Right::SendOnce { address }) => Some(*address),
        _ => None,
    }
}

/// Take a receive right out of the space so its descriptor can be sent
/// elsewhere - `MACH_MSG_TYPE_MOVE_RECEIVE`.
///
/// Fails when something else still holds the port: a member of a port set, or a
/// receive in progress on another thread. Mach would refuse the same move for
/// the same reason, and moving it anyway would leave this process reading a
/// socket another process now owns.
pub fn move_receive(name: Name) -> Result<Receiver, i32> {
    let taken = with(|space| match space.names.get(&name) {
        Some(Right::Receive(_)) => space.names.remove(&name),
        Some(_) => None,
        None => None,
    });
    let Some(Right::Receive(port)) = taken else {
        return Err(MACH_SEND_INVALID_RIGHT);
    };
    match Arc::try_unwrap(port) {
        Ok(port) => Ok(port),
        Err(shared) => {
            // Put it back: the caller still holds the right it asked to move.
            with(|space| space.names.insert(name, Right::Receive(shared)));
            Err(MACH_SEND_INVALID_RIGHT)
        }
    }
}

/// Add a send right for `address`, reusing an existing name for the same port.
///
/// Reuse is what Mach does and what callers depend on: two messages carrying a
/// send right to the same port arrive naming the same integer, and a client that
/// compares them - a crash reporter deciding whether it already knows this
/// sender - would otherwise see two ports where there is one.
pub fn insert_send(address: Address) -> Name {
    with(|space| {
        for (name, right) in space.names.iter_mut() {
            if let Right::Send {
                address: existing,
                refs,
            } = right
                && *existing == address
            {
                *refs = refs.saturating_add(1);
                return *name;
            }
        }
        let name = space.next;
        space.next = space.next.wrapping_add(4);
        space.names.insert(name, Right::Send { address, refs: 1 });
        name
    })
}

/// Add a send-once right, which is never shared with an existing name.
pub fn insert_send_once(address: Address) -> Name {
    insert(Right::SendOnce { address })
}

/// Add a receive right this process has just acquired.
pub fn insert_receive(port: Receiver) -> Name {
    insert(Right::Receive(Arc::new(port)))
}

/// Drop one user reference to `name`, removing it when the last one goes.
///
/// This is `mach_port_deallocate`'s whole behaviour and the tail of several
/// others, so it is spelled once.
fn release(space: &mut Space, name: Name) -> i32 {
    let Some(right) = space.names.get_mut(&name) else {
        return KERN_INVALID_NAME;
    };
    match right {
        Right::Send { refs, .. } | Right::Dead { refs } => {
            *refs -= 1;
            if *refs == 0 {
                space.names.remove(&name);
            }
            KERN_SUCCESS
        }
        Right::SendOnce { .. } => {
            space.names.remove(&name);
            KERN_SUCCESS
        }
        // `mach_port_deallocate` on a receive right is an error on a real
        // system, and the mistake it catches - deallocating the port you are
        // about to listen on - is one worth still catching here.
        Right::Receive(_) | Right::Set(_) => KERN_INVALID_RIGHT,
        Right::Semaphore(_) | Right::Special => KERN_SUCCESS,
    }
}

/// Remove `name` and everything behind it, whatever kind of right it is.
pub fn destroy(name: Name) -> i32 {
    with(|space| match space.names.remove(&name) {
        Some(Right::Set(members)) => {
            let _ = members;
            KERN_SUCCESS
        }
        Some(_) => KERN_SUCCESS,
        None => KERN_INVALID_NAME,
    })
}

// ---------------------------------------------------------------------------
// `mach_port_construct` and the `mach_port_options_t` it reads.
//
// **The structure is a struct, not a union, and `flags` is a bitmask of
// independent attributes rather than a tag.** The SDK's `mach/port.h` - the same
// text in 10.15, 11.0 and 11.3, and the same as `osfmk/mach/port.h` - says:
//
// ```c
// typedef struct mach_port_options {
//         uint32_t                flags;    /* Flags defining attributes for port */
//         mach_port_limits_t      mpl;      /* Message queue limit for port */
//         union {
//                 uint64_t        reserved[2];
//                 mach_port_name_t work_interval_port;
//         };
// } mach_port_options_t;
//
// typedef struct mach_port_limits {
//         mach_port_msgcount_t    mpl_qlimit;     /* number of msgs */
// } mach_port_limits_t;                          /* mach_port_msgcount_t is natural_t */
//
// #define MPO_QLIMIT              0x02
// #define MPO_INSERT_SEND_RIGHT   0x10
// ```
//
// 24 bytes: `flags` at +0, `mpl.mpl_qlimit` at +4, and sixteen bytes of
// `reserved` after that. **Both of the fields a caller writes are one 32-bit
// word**, which is the whole of the puzzle below. There is no
// `MPO_EXTRACT_SEND_RIGHT`, no `MPO_MOVE_*` and no `mach_port_port_t` in the
// header any more: the insert/extract/move encoding an earlier draft of this
// file assumed is not one Darwin declares, and the `0x8` that draft gave
// `MPO_INSERT_SEND_RIGHT` is `MPO_IMPORTANCE_RECEIVER`.
//
// ## The encoding, read out of the guest
//
// A header is not evidence about a binary that was compiled against a possibly
// different one, so the layout was read from Chromium 130's Framework itself
// (`test/Chromium/.../Chromium Framework`, 244 MB; `test/tools/macho.py` parses
// it, `test/tools/raw-range.py` disassembles a range of it). It imports
// `_mach_port_construct` through the stub at `0xcca167a` and has exactly two
// `call`s to that stub in the whole image; no other Mach-O in the app bundle
// imports it at all.
//
// **1. `WaitableEvent::WaitableEvent` at `0x5d9a090`**, the constructor whose
// `CHECK` is the one that fires: the failure path calls `Check(file, line,
// condition)` at `0x5d9a2a0` with `line = 0x27` = 39 and
// `"./../base/synchronization/waitable_event_apple.cc"`, so the two statements
// below are lines 32 to 38:
//
// ```text
//  5d9a0ce  xorps  xmm0,xmm0
//  5d9a0d1  movups XMMWORD PTR [rbp-0x50],xmm0    ; 16 bytes of `reserved`, zeroed
//  5d9a0d5  movabs rax,0x100000010
//  5d9a0df  mov    QWORD PTR [rbp-0x58],rax      ; <- the option word
//  5d9a0e3  mov    DWORD PTR [rbp-0x2c],0xaaaaaaaa   ; mach_port_t name, unwritten
//  5d9a0f5  lea    rsi,[rbp-0x58]                 ; &options
//  5d9a0f9  lea    rcx,[rbp-0x2c]                 ; &name
//  5d9a0fd  xor    edx,edx                        ; context
//  5d9a0ff  call   _mach_port_construct
// ```
//
// which is
//
// ```c
// mach_port_options_t options{};
// options.flags = MPO_INSERT_SEND_RIGHT;
// options.mpl.mpl_qlimit = 1;
// ```
//
// **So `0x100000010` is not a Mach port name and not a Mach port of any kind.**
// It is two adjacent 32-bit fields of one structure, read across as a single
// 64-bit word: the low half is `flags` and the high half is `mpl.mpl_qlimit`,
// and the compiler folded two constant stores into one. A `mach_port_name_t` is
// 32 bits wide, so no Mach name can have bit 32 set at all - the shape
// `(1 << 32) | index` is not a name this kernel, or this namespace, can issue.
//
// **2. `base::apple::CreateMachPort` at `0x5d94340`**, which builds the same
// word out of runtime values and is what fixes the two field widths and the
// two constants independently of the folded store above:
//
// ```text
//  5d94362  xorps  xmm0,xmm0
//  5d94365  movaps XMMWORD PTR [rbp-0x60],xmm0    ; 16 bytes, zeroed
//  5d94369  mov    QWORD PTR [rbp-0x50],0x0       ; ...all 24
//  5d9435c  test   rsi,rsi                        ; send != nullptr
//  5d94371  shl    eax,0x4                        ; << 4 = MPO_INSERT_SEND_RIGHT
//  5d94374  mov    DWORD PTR [rbp-0x60],eax       ; options.flags, 4 bytes at +0
//  5d94377  bt     rdx,0x20                       ; queue_limit.has_value()
//  5d9437e  or     eax,0x2                        ; MPO_QLIMIT
//  5d94381  mov    DWORD PTR [rbp-0x60],eax
//  5d94384  mov    DWORD PTR [rbp-0x5c],edx       ; options.mpl.mpl_qlimit at +4
// ```
//
// from `base/apple/scoped_mach_port.cc:40-46`. A 4-byte store at +0 and a
// 4-byte store at +4, with the shift fixing `MPO_INSERT_SEND_RIGHT` at `0x10`
// and the or fixing `MPO_QLIMIT` at `0x2` - the values `<mach/port.h>` gives
// them. **Both call sites in the entire guest are served below.**

/// `mach_port_construct`'s `options.flags`: set the queue limit for the new
/// port's message queue, from `options.mpl.mpl_qlimit`.
///
/// Accepted and the value dropped - see [`mach_port_construct`]. The other
/// attribute bits are named in [`ATTRIBUTES`] because a refusal that says which
/// bit it refused is a refusal a caller can act on.
pub const MPO_QLIMIT: u32 = 0x2;

/// `mach_port_construct`'s `options.flags`: also insert a send right to the new
/// port, so the caller can send on it as well as receive.
///
/// `0x10`, not the `0x8` an earlier draft of this file carried: that draft read
/// `mach_port_options_t` as a union, and `0x8` is `MPO_IMPORTANCE_RECEIVER`.
pub const MPO_INSERT_SEND_RIGHT: u32 = 0x10;

/// Every attribute bit `options.flags` may carry: `MPO_OPTIONS_MASK`.
pub const MPO_OPTIONS_MASK: u32 = 0x23ff;

/// The port-type bits of `options.flags`: `MPO_PORT_TYPE_MASK`. The type is
/// `MPO_PORT`, zero, for a plain port - so `flags == 0` is a complete request
/// for one, and is served.
pub const MPO_PORT_TYPE_MASK: u32 = 0x1dc00;

/// `MPO_UNUSED_BITS`: everything outside the two masks above. Mach checks this
/// first and answers `KERN_INVALID_ARGUMENT`, and so does [`mach_port_construct`].
pub const MPO_UNUSED_BITS: u32 = !(MPO_OPTIONS_MASK | MPO_PORT_TYPE_MASK);

/// The attribute bits, by name, in `<mach/port.h>`'s order. Carries no logic:
/// it is the message a refusal speaks, and the test that the mask above is
/// exactly the union of the bits in it.
const ATTRIBUTES: &[(u32, &str)] = &[
    (0x01, "MPO_CONTEXT_AS_GUARD"),
    (MPO_QLIMIT, "MPO_QLIMIT"),
    (0x04, "MPO_TEMPOWNER"),
    (0x08, "MPO_IMPORTANCE_RECEIVER"),
    (MPO_INSERT_SEND_RIGHT, "MPO_INSERT_SEND_RIGHT"),
    (0x20, "MPO_STRICT"),
    (0x40, "MPO_DENAP_RECEIVER"),
    (0x80, "MPO_IMMOVABLE_RECEIVE"),
    (0x100, "MPO_FILTER_MSG"),
    (0x200, "MPO_TG_BLOCK_TRACKING"),
    (0x2000, "MPO_ENFORCE_REPLY_PORT_SEMANTICS"),
];

/// The port types, by name. `MPO_PORT` is 0 and so is not here: it is the one
/// type this namespace answers, and `mach_port_type` then reports an ordinary
/// receive right.
const PORT_TYPES: &[(u32, &str)] = &[
    (0x400, "MPO_SERVICE_PORT"),
    (0x800, "MPO_CONNECTION_PORT"),
    (0x1000, "MPO_REPLY_PORT"),
    (0x4000, "MPO_PROVISIONAL_REPLY_PORT"),
    (0x8000, "MPO_EXCEPTION_PORT"),
    (0x10000, "MPO_CONNECTION_PORT_WITH_PORT_ARRAY"),
];

/// The named bits of `bits`, for a message: nothing for no bits, since a
/// caller reading "asks for " has to be able to see the end of the list. Bits
/// that name nothing stay as their number, because inventing a name for one is
/// how a diagnostic starts lying.
fn named(bits: u32, names: &[(u32, &str)]) -> String {
    if bits == 0 {
        return String::new();
    }
    let mut left = bits;
    let mut out: Vec<&str> = Vec::new();
    for (bit, name) in names {
        if left & bit != 0 {
            out.push(name);
            left &= !bit;
        }
    }
    match out.is_empty() {
        true => format!("{left:#x}"),
        false if left == 0 => out.join("|"),
        false => format!("{}|{left:#x}", out.join("|")),
    }
}

/// `kern_return_t mach_port_construct(ipc_space_t, mach_port_options_t *,
/// uint64_t context, mach_port_t *)`
///
/// Creates a port and hands the caller a name for it in one call. There is no
/// two-argument form: the options and the context are always there, which is
/// worth recording because a two-argument prototype reads `options` as the
/// name and answers about a value the caller never put there.
///
/// **What is served, and what is not.** The one request this answers is an
/// ordinary port - `MPO_PORT`, which is to say `options.flags == 0` - with
/// nothing else asked of it, or with [`MPO_INSERT_SEND_RIGHT`] and/or
/// [`MPO_QLIMIT`] alongside:
///
/// * Mach's answer to `MPO_INSERT_SEND_RIGHT` is `IP_INIT_MAKE_SEND_RIGHT`:
///   one entry in the space whose type is receive *and* send
///   (`ipc_port_alloc` sets `type = MACH_PORT_TYPE_RECEIVE | MACH_PORT_TYPE_SEND`
///   under one name), which is why a caller may send on what it just received
///   and why Chromium assigns the one name to both of its handles. Not a
///   special case here: a receive right in this namespace is already a legal
///   message destination ([`destination`]), so the single right this inserts
///   serves both. The one difference worth naming is that `mach_port_type`
///   reports an ordinary receive right where a real kernel reports
///   `MACH_PORT_TYPE(RECEIVE) | MACH_PORT_TYPE(SEND)`, because this namespace
///   has one right per name and a name that means two things cannot be
///   reference counted as two.
/// * `MPO_QLIMIT` is accepted and its value dropped. Mach sets the depth of a
///   queue that does not exist here, and the only thing a caller can observe
///   about a queue's depth is what `mach_msg` returns to the *receiver* - which
///   in this namespace is the empty message Chromium sends as the signal, not
///   the number of messages waiting. Chromium's own reason for asking for a
///   limit of one is that a second send to an already-signalled event should
///   block rather than succeed, and blocking is what a send to a socket here
///   does once the receiver is not waiting.
///
/// Everything else is refused **by name**, and that is the point rather than a
/// limitation of convenience. `mach_port_construct` can also be asked for a
/// guarded port, a port with a temp owner the kernel deallocates when the task
/// dies, an importance or app-nap donation receiver, an immovable receive
/// right, a message filter, thread-group blocking tracking, reply-port
/// semantics, or any of the six port types that are not a plain port - a
/// service port launchd can look up, a connection port, an exception port.
/// Answering any of those with a fresh receive right hands the caller a port it
/// did not ask for and silently does not do what it said, and a wrong answer is
/// invisible where a refusal names itself. Both of Chromium's call sites are in
/// the served set, which is what lets the remaining nine attribute bits and all
/// six other port types be refused honestly instead of waved through.
///
/// `context` is Mach's value to associate with the port, which a real kernel
/// keeps on the port itself (`port->ip_context = context`) for the guard
/// machinery. Nothing in this namespace has anywhere to put one and nothing
/// here reads one back, so it is accepted and dropped rather than refused for
/// being present. The one option that gives it meaning, `MPO_CONTEXT_AS_GUARD`,
/// is refused above.
///
/// # Safety
///
/// `options` is null or a readable `mach_port_options_t`; `name` is a writable
/// `mach_port_name_t`.
pub unsafe extern "C" fn mach_port_construct(
    task: Name,
    options: *mut c_void,
    context: u64,
    name: *mut Name,
) -> i32 {
    if name.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    if task != TASK_SELF {
        // Constructing a right in *another* task's space is a privilege this
        // process does not have and the runtime cannot express: the namespace
        // here is one flat space for this process, so there is no other task's
        // to insert into. Mach refuses a task that is not this one too.
        return KERN_INVALID_TASK;
    }
    // `flags` and `mpl.mpl_qlimit`, the two fields a caller writes, each one
    // 32-bit value - see the note above this file's first `MPO_` constant for how
    // that was established from the guest's own stores. A null `options` is the
    // same request as a zeroed structure, because every bit in `flags` is opt-in
    // and `MPO_PORT` is zero.
    let (flags, _qlimit) = if options.is_null() {
        (0, 0)
    } else {
        // SAFETY: the caller states `options` is readable, and the first two
        // fields of the structure are 32-bit integers.
        unsafe {
            let fields = options.cast::<u32>();
            (fields.read(), fields.add(1).read())
        }
    };
    let port_type = flags & MPO_PORT_TYPE_MASK;
    let attributes = flags & MPO_OPTIONS_MASK;
    // Mach's own first test is `options->flags & MPO_UNUSED_BITS`; the two after
    // it are this file's, for the requests it does not do. The queue limit is
    // read and dropped on purpose: there is no queue here for a limit to bound,
    // and the comment above says what a caller can and cannot observe about that.
    if flags & MPO_UNUSED_BITS != 0
        || port_type != 0
        || attributes & !(MPO_INSERT_SEND_RIGHT | MPO_QLIMIT) != 0
    {
        return refused_construct(flags, attributes, port_type, context, task, options, name);
    }
    let port = match Receiver::bind() {
        Ok(port) => port,
        Err(_) => return KERN_RESOURCE_SHORTAGE,
    };
    let inserted = insert_receive(port);
    // SAFETY: the caller states `name` is a writable `mach_port_name_t`.
    unsafe { name.write(inserted) };
    KERN_SUCCESS
}

/// Say once that a construct asked for something this does not do, and say what.
///
/// The words are in the message because a caller that only checks the return
/// code cannot say anything else: `KERN_INVALID_ARGUMENT` is a number, and
/// Chromium's own report of it names only its file and line. Naming the *bits*
/// is what makes the number actionable - `MPO_SERVICE_PORT` can be looked up in
/// `<mach/port.h>` and reasoned about, `0x400` cannot.
fn refused_construct(
    flags: u32,
    attributes: u32,
    port_type: u32,
    context: u64,
    task: Name,
    options: *mut c_void,
    name: *mut Name,
) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        let mut asked = named(flags & MPO_UNUSED_BITS, &[]);
        if !asked.is_empty() {
            asked.push_str(", which mach_port_construct does not define at all");
        }
        let bits = named(
            attributes & !(MPO_INSERT_SEND_RIGHT | MPO_QLIMIT),
            ATTRIBUTES,
        );
        if !bits.is_empty() {
            if !asked.is_empty() {
                asked.push_str(", and ");
            }
            asked.push_str(&bits);
        }
        if port_type != 0 {
            if !asked.is_empty() {
                asked.push_str(", and ");
            }
            asked.push_str(&format!("port type {}", named(port_type, PORT_TYPES)));
        }
        eprintln!(
            "rosetta3: mach_port_construct: task={task:#x} options={options:p} \
             context={context:#x} name={name:p}; options.flags={flags:#x} asks for {asked}, \
             and only MPO_INSERT_SEND_RIGHT ({MPO_INSERT_SEND_RIGHT:#x}) and MPO_QLIMIT \
             ({MPO_QLIMIT:#x}) are answered. A name in this namespace is one entry in one flat \
             table, so a guard, a temp owner to deallocate, an app-nap or importance donation, \
             a message filter, reply-port semantics and a launchd to look a service port up in \
             are all outside what a construct can give a caller here"
        );
    });
    KERN_INVALID_ARGUMENT
}

/// `kern_return_t mach_port_allocate(ipc_space_t, mach_port_right_t, mach_port_name_t *)`
///
/// # Safety
///
/// `out` is a writable `mach_port_name_t`.
pub unsafe extern "C" fn mach_port_allocate(task: Name, right: u32, out: *mut Name) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    let name = match right {
        RIGHT_RECEIVE => match Receiver::bind() {
            Ok(port) => insert_receive(port),
            Err(_) => return KERN_RESOURCE_SHORTAGE,
        },
        RIGHT_PORT_SET => insert(Right::Set(Vec::new())),
        // A dead name with no port behind it; Mach allows this and nothing this
        // runtime has met asks for it, but the alternative is answering an
        // unknown right with a receive right, which is a live port a caller
        // would then listen on.
        RIGHT_DEAD_NAME => insert(Right::Dead { refs: 1 }),
        _ => return KERN_INVALID_VALUE,
    };
    // SAFETY: the caller's `mach_port_name_t`.
    unsafe { out.write(name) };
    KERN_SUCCESS
}

/// `kern_return_t mach_port_deallocate(ipc_space_t, mach_port_name_t)`
pub extern "C" fn mach_port_deallocate(task: Name, name: Name) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    if name == MACH_PORT_NULL || name == MACH_PORT_DEAD {
        return KERN_SUCCESS;
    }
    with(|space| release(space, name))
}

/// `kern_return_t mach_port_destroy(ipc_space_t, mach_port_name_t)`
pub extern "C" fn mach_port_destroy(task: Name, name: Name) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    destroy(name)
}

/// `kern_return_t mach_port_mod_refs(ipc_space_t, mach_port_name_t,
/// mach_port_right_t, mach_port_delta_t)`
pub extern "C" fn mach_port_mod_refs(task: Name, name: Name, right: u32, delta: i32) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    with(|space| {
        let Some(held) = space.names.get_mut(&name) else {
            return KERN_INVALID_NAME;
        };
        if held.type_bits() != type_of(right) {
            return KERN_INVALID_RIGHT;
        }
        match held {
            Right::Send { refs, .. } | Right::Dead { refs } => {
                let updated = *refs as i64 + delta as i64;
                if updated < 0 {
                    return KERN_INVALID_VALUE;
                }
                if updated > u32::MAX as i64 {
                    return KERN_UREFS_OVERFLOW;
                }
                *refs = updated as u32;
                if updated == 0 {
                    space.names.remove(&name);
                }
                KERN_SUCCESS
            }
            // A receive right and a port set have exactly one reference, so the
            // only legal delta is the one that destroys them.
            _ => match delta {
                0 => KERN_SUCCESS,
                -1 => {
                    space.names.remove(&name);
                    KERN_SUCCESS
                }
                _ => KERN_INVALID_VALUE,
            },
        }
    })
}

/// `kern_return_t mach_port_get_refs(ipc_space_t, mach_port_name_t,
/// mach_port_right_t, mach_port_urefs_t *)`
///
/// # Safety
///
/// `out` is a writable `mach_port_urefs_t`.
pub unsafe extern "C" fn mach_port_get_refs(
    task: Name,
    name: Name,
    right: u32,
    out: *mut u32,
) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    let guard = space().read().unwrap_or_else(|e| e.into_inner());
    let Some(held) = guard.names.get(&name) else {
        return KERN_INVALID_NAME;
    };
    let refs = match (held, held.type_bits() == type_of(right)) {
        (_, false) => 0,
        (Right::Send { refs, .. } | Right::Dead { refs }, true) => *refs,
        (_, true) => 1,
    };
    // SAFETY: the caller's `mach_port_urefs_t`.
    unsafe { out.write(refs) };
    KERN_SUCCESS
}

/// `kern_return_t mach_port_insert_right(ipc_space_t, mach_port_name_t,
/// mach_port_poly_t, mach_msg_type_name_t)`
///
/// The name is the caller's choice, not this space's, which is the one place a
/// caller writes into the namespace rather than reading a name out of it.
pub extern "C" fn mach_port_insert_right(
    task: Name,
    name: Name,
    right: Name,
    disposition: u32,
) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    let Some(address) = address_of(right) else {
        return KERN_INVALID_CAPABILITY;
    };
    with(|space| {
        if space.names.contains_key(&name) && name != right {
            return KERN_RIGHT_EXISTS;
        }
        let inserted = match disposition {
            super::msg::MAKE_SEND | super::msg::COPY_SEND | super::msg::MOVE_SEND => {
                Right::Send { address, refs: 1 }
            }
            super::msg::MAKE_SEND_ONCE | super::msg::MOVE_SEND_ONCE => Right::SendOnce { address },
            _ => return KERN_INVALID_VALUE,
        };
        space.names.insert(name, inserted);
        KERN_SUCCESS
    })
}

/// `kern_return_t mach_port_extract_right(ipc_space_t, mach_port_name_t,
/// mach_msg_type_name_t, mach_port_poly_t *, mach_msg_type_name_t *)`
///
/// The call a crash reporter makes to keep a send right to the port whose
/// receive right it is about to hand its handler.
///
/// # Safety
///
/// `out` and `out_type` are writable 32-bit words, or null.
pub unsafe extern "C" fn mach_port_extract_right(
    task: Name,
    name: Name,
    disposition: u32,
    out: *mut Name,
    out_type: *mut u32,
) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    let Some(address) = address_of(name) else {
        return KERN_INVALID_NAME;
    };
    let (extracted, kind) = match disposition {
        super::msg::MAKE_SEND | super::msg::COPY_SEND => {
            (insert_send(address), super::msg::MOVE_SEND)
        }
        super::msg::MAKE_SEND_ONCE => (insert_send_once(address), super::msg::MOVE_SEND_ONCE),
        super::msg::MOVE_SEND => (name, super::msg::MOVE_SEND),
        _ => return KERN_INVALID_VALUE,
    };
    if !out.is_null() {
        // SAFETY: the caller's `mach_port_t`.
        unsafe { out.write(extracted) };
    }
    if !out_type.is_null() {
        // SAFETY: the caller's `mach_msg_type_name_t`.
        unsafe { out_type.write(kind) };
    }
    KERN_SUCCESS
}

/// `kern_return_t mach_port_type(ipc_space_t, mach_port_name_t, mach_port_type_t *)`
///
/// # Safety
///
/// `out` is a writable `mach_port_type_t`.
pub unsafe extern "C" fn mach_port_type(task: Name, name: Name, out: *mut u32) -> i32 {
    if out.is_null() {
        return KERN_INVALID_ADDRESS;
    }
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    let guard = space().read().unwrap_or_else(|e| e.into_inner());
    let Some(held) = guard.names.get(&name) else {
        return KERN_INVALID_NAME;
    };
    // SAFETY: the caller's `mach_port_type_t`.
    unsafe { out.write(held.type_bits()) };
    KERN_SUCCESS
}

/// `kern_return_t mach_port_insert_member(ipc_space_t, mach_port_name_t,
/// mach_port_name_t pset)`
pub extern "C" fn mach_port_insert_member(task: Name, name: Name, pset: Name) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    if receiver(name).is_none() {
        return KERN_INVALID_RIGHT;
    }
    with(|space| match space.names.get_mut(&pset) {
        Some(Right::Set(members)) => {
            if !members.contains(&name) {
                members.push(name);
            }
            KERN_SUCCESS
        }
        Some(_) => KERN_INVALID_RIGHT,
        None => KERN_INVALID_NAME,
    })
}

/// `kern_return_t mach_port_move_member(ipc_space_t, mach_port_name_t,
/// mach_port_name_t after)`
///
/// A member belongs to at most one set, so this is a removal from every set
/// followed by an insertion - and `MACH_PORT_NULL` for `after` is the removal
/// on its own.
pub extern "C" fn mach_port_move_member(task: Name, name: Name, after: Name) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    if receiver(name).is_none() {
        return KERN_INVALID_RIGHT;
    }
    with(|space| {
        for right in space.names.values_mut() {
            if let Right::Set(members) = right {
                members.retain(|member| *member != name);
            }
        }
        if after == MACH_PORT_NULL {
            return KERN_SUCCESS;
        }
        match space.names.get_mut(&after) {
            Some(Right::Set(members)) => {
                members.push(name);
                KERN_SUCCESS
            }
            Some(_) => KERN_INVALID_RIGHT,
            None => KERN_INVALID_NAME,
        }
    })
}

/// `kern_return_t mach_port_get_set_status(...)` and the attribute calls beside
/// it, which set queue limits and other things this transport does not model.
///
/// Accepted rather than refused: every one of them is advisory - a caller that
/// is told its queue limit was raised behaves the same either way - and the
/// alternative is failing an initialisation that had nothing wrong with it.
pub extern "C" fn mach_port_set_attributes(
    task: Name,
    name: Name,
    _flavor: i32,
    _info: *mut std::ffi::c_void,
    _count: u32,
) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    let guard = space().read().unwrap_or_else(|e| e.into_inner());
    match guard.names.contains_key(&name) {
        true => KERN_SUCCESS,
        false => KERN_INVALID_NAME,
    }
}

/// `kern_return_t mach_port_request_notification(ipc_space_t, mach_port_name_t,
/// mach_msg_id_t, mach_port_mscount_t, mach_port_name_t notify,
/// mach_msg_type_name_t, mach_port_name_t *previous)`
///
/// Recorded and never delivered. There is no kernel here watching a port to
/// notice that its last send right went away, and inventing the notification
/// would be worse than withholding it: a dead-name notification tells a client
/// its server has died, so a spurious one makes a working server look dead. The
/// withheld direction says "still alive", which is the failure a caller can
/// survive.
///
/// # Safety
///
/// `previous` is a writable `mach_port_name_t`, or null.
pub unsafe extern "C" fn mach_port_request_notification(
    task: Name,
    name: Name,
    _id: i32,
    _sync: u32,
    notify: Name,
    _disposition: u32,
    previous: *mut Name,
) -> i32 {
    if task != TASK_SELF {
        return KERN_INVALID_TASK;
    }
    {
        let guard = space().read().unwrap_or_else(|e| e.into_inner());
        if !guard.names.contains_key(&name) {
            return KERN_INVALID_NAME;
        }
    }
    // The send-once right the caller handed over is consumed by the request,
    // whether or not the notification ever fires.
    if notify != MACH_PORT_NULL {
        with(|space| release(space, notify));
    }
    if !previous.is_null() {
        // SAFETY: the caller's `mach_port_name_t`.
        unsafe { previous.write(MACH_PORT_NULL) };
    }
    announce_undelivered_notifications();
    KERN_SUCCESS
}

fn announce_undelivered_notifications() {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!(
            "rosetta3: mach_port_request_notification accepted but never delivered - nothing here \
             watches a port's reference counts, so a caller waiting for a dead-name notification \
             waits forever rather than being told a live port died"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mistake this constant exists to stop: `MACH_PORT_RIGHT_SEND` is
    /// zero and `MACH_PORT_RIGHT_RECEIVE` is one, so a table indexed the
    /// intuitive way round hands out a send right to a caller that asked to
    /// receive.
    #[test]
    fn the_right_numbers_are_the_headers_and_not_the_intuitive_order() {
        assert_eq!(RIGHT_SEND, 0);
        assert_eq!(RIGHT_RECEIVE, 1);
        assert_eq!(type_of(RIGHT_SEND), 0x0001_0000);
        assert_eq!(type_of(RIGHT_RECEIVE), 0x0002_0000);
        assert_eq!(type_of(RIGHT_DEAD_NAME), 0x0010_0000);
    }

    /// The two masks `mach_port_construct` tests are exactly the union of the
    /// bits named beside them, so a bit added to a table without being added to
    /// a mask is caught here rather than becoming a construct this file refuses
    /// for no reason it can name.
    #[test]
    fn the_option_masks_are_the_union_of_the_bits_named_beside_them() {
        let attributes = ATTRIBUTES.iter().fold(0, |mask, (bit, _)| mask | bit);
        assert_eq!(attributes, MPO_OPTIONS_MASK);
        let port_types = PORT_TYPES.iter().fold(0, |mask, (bit, _)| mask | bit);
        assert_eq!(port_types, MPO_PORT_TYPE_MASK);
        assert_eq!(MPO_UNUSED_BITS & (MPO_OPTIONS_MASK | MPO_PORT_TYPE_MASK), 0);
    }

    /// The message a refusal speaks is the whole of what a caller gets to know
    /// about a construct this file will not do, so it has to name the bit and
    /// nothing but the bit.
    #[test]
    fn a_refusal_names_the_bits_it_refused() {
        assert_eq!(named(0, ATTRIBUTES), "", "no bits, no list");
        assert_eq!(named(MPO_QLIMIT, ATTRIBUTES), "MPO_QLIMIT");
        assert_eq!(named(0x04, ATTRIBUTES), "MPO_TEMPOWNER");
        assert_eq!(
            named(0x04 | 0x08, ATTRIBUTES),
            "MPO_TEMPOWNER|MPO_IMPORTANCE_RECEIVER"
        );
        assert_eq!(named(0x400, PORT_TYPES), "MPO_SERVICE_PORT");
        assert_eq!(
            named(0x400 | 0x1_0000, PORT_TYPES),
            "MPO_SERVICE_PORT|MPO_CONNECTION_PORT_WITH_PORT_ARRAY"
        );
        // A bit with no name in the table stays its number: it is not this
        // file's job to guess, and a wrong name is worse than none.
        assert_eq!(named(0x4000_0000, ATTRIBUTES), "0x40000000");
    }

    /// The two stores Chromium's `WaitableEvent` constructor makes
    /// (`Framework+0x5d9a0df`, from `options.flags = MPO_INSERT_SEND_RIGHT`
    /// and `options.mpl.mpl_qlimit = 1`) and the guest word the compiler folds
    /// them into. Read back the way a guest would read it: 64 bits at once.
    #[test]
    fn the_option_word_chromium_writes_is_two_adjacent_u32_fields() {
        let mut block = [0u8; 24];
        block[0..4].copy_from_slice(&MPO_INSERT_SEND_RIGHT.to_ne_bytes());
        block[4..8].copy_from_slice(&1u32.to_ne_bytes());
        let word = u64::from_ne_bytes(block[0..8].try_into().unwrap());
        assert_eq!(
            word, 0x0000_0001_0000_0010,
            "the guest's movabs immediate is flags in the low half and mpl.mpl_qlimit in the high one"
        );

        // The halves are not interchangeable, and that is the layout rather than
        // an accident of the fold: a word with the two fields the other way
        // round is one this file refuses, because `MPO_INSERT_SEND_RIGHT` in the
        // high half is a bit `mach_port_construct` does not define at all.
        block[0..8].copy_from_slice(&0x0000_0010_0000_0001u64.to_ne_bytes());
        let mut name = 0xaaaa_aaaau32;
        // SAFETY: `block` is a live 24-byte structure and `name` a live `Name`.
        let rc = unsafe { mach_port_construct(TASK_SELF, block.as_mut_ptr().cast(), 0, &mut name) };
        assert_eq!(rc, KERN_INVALID_ARGUMENT, "the halves are not swappable");
        assert_eq!(name, 0xaaaa_aaaa, "and no name was invented for it");
    }

    /// What a guest observes for Chromium's own construct: the word the guest
    /// stores is accepted, and the name it gets back is what its two handles
    /// need - a receive right it can wait on, and a destination it can send to.
    #[test]
    fn the_construct_chromium_performs_gives_a_name_it_can_wait_on_and_send_to() {
        // The guest's exact 24 bytes: its folded 8-byte store, then the 16
        // bytes of `reserved` it zeroed and never touched again.
        let mut block = [0u8; 24];
        block[0..8].copy_from_slice(&0x0000_0001_0000_0010u64.to_ne_bytes());

        let mut name = 0xaaaa_aaaau32;
        // SAFETY: `block` is a live 24-byte structure and `name` a live `Name`.
        let rc = unsafe { mach_port_construct(TASK_SELF, block.as_mut_ptr().cast(), 0, &mut name) };
        assert_eq!(rc, KERN_SUCCESS, "options.flags = MPO_INSERT_SEND_RIGHT");
        assert_ne!(name, 0xaaaa_aaaa, "the name was written");
        assert_ne!(name, MACH_PORT_NULL);

        // `receive_right_ = new ReceiveRight(name)`
        let mut bits = 0;
        // SAFETY: `bits` is a live `mach_port_type_t`.
        assert_eq!(
            unsafe { mach_port_type(TASK_SELF, name, &mut bits) },
            KERN_SUCCESS
        );
        assert_eq!(bits, type_of(RIGHT_RECEIVE));
        // ... and `send_right_.reset(name)`, which needs the same name to be a
        // legal destination.
        assert!(
            destination(name).is_ok(),
            "Chromium sends an empty message to this name to signal"
        );
        mach_port_destroy(TASK_SELF, name);
    }

    /// `base::apple::CreateMachPort`'s other shape: no send right at all
    /// (`options.flags = 0`, Mach's `MPO_PORT`), and a queue limit with it.
    /// Both are the whole of what the two call sites in the guest ask for.
    #[test]
    fn a_plain_port_with_a_queue_limit_is_constructed() {
        for (flags, qlimit) in [
            (0u32, 0u32),
            (MPO_INSERT_SEND_RIGHT, 0),
            (MPO_QLIMIT, 1),
            (MPO_INSERT_SEND_RIGHT | MPO_QLIMIT, 5),
        ] {
            let mut block = [0u8; 24];
            block[0..4].copy_from_slice(&flags.to_ne_bytes());
            block[4..8].copy_from_slice(&qlimit.to_ne_bytes());
            let mut name = MACH_PORT_NULL;
            // SAFETY: a live structure and a live `Name`.
            let rc =
                unsafe { mach_port_construct(TASK_SELF, block.as_mut_ptr().cast(), 0, &mut name) };
            assert_eq!(rc, KERN_SUCCESS, "flags {flags:#x} qlimit {qlimit}");
            assert_ne!(name, MACH_PORT_NULL);
            mach_port_destroy(TASK_SELF, name);
        }
    }

    /// The standard a refusal has to meet: a caller must be able to tell that it
    /// got nothing. So a construct this file will not do leaves the out-parameter
    /// exactly as the caller left it - Chromium's uninitialised
    /// `mach_port_t name;` - and says so in its return code.
    #[test]
    fn a_construct_asking_for_something_else_is_refused_and_writes_no_name() {
        // A service port, which is the one named here: a real kernel registers
        // it with launchd, and this namespace has no launchd and no way to
        // tell a caller its port was not that.
        for flags in [
            MPO_INSERT_SEND_RIGHT | 0x400,
            0x04,
            0x01,
            0x20,
            0x80,
            0x2000,
            0x4000_0000,
        ] {
            let mut block = [0u8; 24];
            block[0..4].copy_from_slice(&flags.to_ne_bytes());
            let mut name = 0xaaaa_aaaau32;
            // SAFETY: a live structure and a live `Name`.
            let rc =
                unsafe { mach_port_construct(TASK_SELF, block.as_mut_ptr().cast(), 0, &mut name) };
            assert_eq!(rc, KERN_INVALID_ARGUMENT, "flags {flags:#x}");
            assert_eq!(name, 0xaaaa_aaaa, "no name was invented for {flags:#x}");
        }
    }

    /// A construct in another task's space is refused before the options are
    /// read, and a null `name` is refused before anything else.
    #[test]
    fn a_construct_needs_a_task_and_a_place_to_put_the_name() {
        // SAFETY: a null out-parameter is the case being tested.
        assert_eq!(
            unsafe {
                mach_port_construct(TASK_SELF, std::ptr::null_mut(), 0, std::ptr::null_mut())
            },
            KERN_INVALID_ADDRESS
        );
        let mut name = MACH_PORT_NULL;
        // SAFETY: a null `options` is a legal request and `name` is live.
        let rc = unsafe { mach_port_construct(HOST_SELF, std::ptr::null_mut(), 0, &mut name) };
        assert_eq!(rc, KERN_INVALID_TASK);
    }

    #[test]
    fn allocating_a_receive_right_gives_a_name_that_reports_itself_as_one() {
        let mut name = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        assert_eq!(
            unsafe { mach_port_allocate(TASK_SELF, RIGHT_RECEIVE, &mut name) },
            KERN_SUCCESS
        );
        assert_ne!(name, MACH_PORT_NULL);

        let mut bits = 0;
        assert_eq!(
            unsafe { mach_port_type(TASK_SELF, name, &mut bits) },
            KERN_SUCCESS
        );
        assert_eq!(bits, type_of(RIGHT_RECEIVE));

        // A receive right cannot be deallocated, only destroyed - the check
        // that catches "deallocate the port you are about to listen on".
        assert_eq!(mach_port_deallocate(TASK_SELF, name), KERN_INVALID_RIGHT);
        assert_eq!(mach_port_destroy(TASK_SELF, name), KERN_SUCCESS);
        assert_eq!(mach_port_destroy(TASK_SELF, name), KERN_INVALID_NAME);
    }

    /// Crashpad's sequence: allocate a receive right, extract a send right to
    /// keep, hand the receive right away.
    #[test]
    fn a_send_right_extracted_from_a_receive_right_names_the_same_port() {
        let mut receive = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { mach_port_allocate(TASK_SELF, RIGHT_RECEIVE, &mut receive) };

        let (mut send, mut kind) = (MACH_PORT_NULL, 0);
        // SAFETY: two live locals for the out-parameters.
        let rc = unsafe {
            mach_port_extract_right(
                TASK_SELF,
                receive,
                super::super::msg::MAKE_SEND,
                &mut send,
                &mut kind,
            )
        };
        assert_eq!(rc, KERN_SUCCESS);
        assert_eq!(kind, super::super::msg::MOVE_SEND);
        assert_ne!(send, receive);
        assert_eq!(address_of(send), address_of(receive));
        assert_eq!(destination(send).unwrap().1, false, "not send-once");

        mach_port_destroy(TASK_SELF, receive);
        mach_port_deallocate(TASK_SELF, send);
    }

    #[test]
    fn a_send_right_is_reference_counted_and_the_name_survives_until_the_last_one() {
        let mut receive = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { mach_port_allocate(TASK_SELF, RIGHT_RECEIVE, &mut receive) };
        let address = address_of(receive).unwrap();

        let first = insert_send(address);
        let second = insert_send(address);
        assert_eq!(first, second, "the same port keeps the same name");

        assert_eq!(mach_port_deallocate(TASK_SELF, first), KERN_SUCCESS);
        assert!(address_of(first).is_some(), "one reference is still held");
        assert_eq!(mach_port_deallocate(TASK_SELF, first), KERN_SUCCESS);
        assert!(address_of(first).is_none());
        mach_port_destroy(TASK_SELF, receive);
    }

    #[test]
    fn a_port_set_holds_the_members_put_into_it() {
        let mut set = MACH_PORT_NULL;
        let mut member = MACH_PORT_NULL;
        // SAFETY: live locals for the out-parameters.
        unsafe {
            mach_port_allocate(TASK_SELF, RIGHT_PORT_SET, &mut set);
            mach_port_allocate(TASK_SELF, RIGHT_RECEIVE, &mut member);
        }
        assert_eq!(
            mach_port_insert_member(TASK_SELF, member, set),
            KERN_SUCCESS
        );
        assert_eq!(members(set).unwrap(), vec![member]);
        // A set is not a receive right, and inserting one into itself is not a
        // question with an answer.
        assert_eq!(
            mach_port_insert_member(TASK_SELF, set, set),
            KERN_INVALID_RIGHT
        );
        assert_eq!(
            mach_port_move_member(TASK_SELF, member, MACH_PORT_NULL),
            KERN_SUCCESS
        );
        assert!(members(set).unwrap().is_empty());

        mach_port_destroy(TASK_SELF, set);
        mach_port_destroy(TASK_SELF, member);
    }

    /// A receive right somebody else still holds cannot be moved out from under
    /// them - the check that stops this process reading a socket another
    /// process now owns.
    #[test]
    fn a_receive_right_in_use_is_not_moved() {
        let mut name = MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { mach_port_allocate(TASK_SELF, RIGHT_RECEIVE, &mut name) };
        let held = receiver(name).expect("a receive right");
        assert_eq!(move_receive(name).err(), Some(MACH_SEND_INVALID_RIGHT));
        assert!(receiver(name).is_some(), "the right was put back");
        drop(held);
        assert!(move_receive(name).is_ok());
    }
}
