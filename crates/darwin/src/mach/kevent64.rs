#![cfg(not(target_os = "macos"))]
//! `kevent64`, and the one filter of it that a Mach port's queue can answer.
//!
//! ## What the guest actually asks for
//!
//! `base::WaitableEvent::WaitManyImpl`
//! (`base/synchronization/waitable_event_apple.cc:130`, read at
//! `refs/branch-heads/6723`, the branch the shipped framework was built from -
//! the `130.0.6723.91` tag has no `mach_port_peek` in that file at all) is one
//! `kevent64` call and nothing else:
//!
//! ```c
//!   std::vector<kevent64_s> events(count);
//!   for (size_t i = 0; i < count; ++i) {
//!     EV_SET64(&events[i], raw_waitables[i]->receive_right_->Name(),
//!              EVFILT_MACHPORT, EV_ADD, 0, 0, i, 0, 0);
//!   }
//!   std::vector<kevent64_s> out_events(count);
//!   ScopedFD wait_many(kqueue());
//!   PCHECK(wait_many.is_valid()) << "kqueue";
//!   int rv = HANDLE_EINTR(kevent64(wait_many.get(), events.data(), count_int,
//!                                  out_events.data(), count_int, /*flags=*/0,
//!                                  /*timeout=*/nullptr));
//!   PCHECK(rv > 0) << "kevent64";
//! ```
//!
//! and the machine code in `Chromium Framework+[0x5d9a7f0, 0x5d9ac3c)` - the
//! whole function is one prologue, the loop that builds the change list, the
//! `kqueue()` at `+0x5d9a990` and the call at `+0x5d9a9db` - agrees with every
//! word of it:
//!
//! ```text
//!  5d9a8b0  mov    rdi,QWORD PTR [r13+rcx*8+0x0]   ; raw_waitables[i]
//!  5d9a8b5  mov    rdi,QWORD PTR [rdi+0x8]          ; -> receive_right_
//!  5d9a8b9  mov    edi,DWORD PTR [rdi+0x10]         ; -> Name(), a 32-bit word
//!  5d9a8bc  mov    QWORD PTR [rsi],rdi              ; ident  = the port name
//!  5d9a8bf  mov    QWORD PTR [rsi+0x8],0x1fff8      ; filter|flags|fflags
//!  5d9a8c7  mov    QWORD PTR [rsi+0x10],0x0        ; data   = 0
//!  5d9a8cf  mov    QWORD PTR [rsi+0x18],rcx        ; udata  = i
//!  5d9a8d3  movaps XMMWORD PTR [rsi+0x20],xmm0     ; ext[0], ext[1] = 0
//! ```
//!
//! The `0x1fff8` is three fields in one eight-byte store, which is the whole of
//! `<sys/event.h>`'s packing read back: `0xfff8` as an `int16_t` is
//! `EVFILT_MACHPORT` (`-8`), `0x0001` as a `uint16_t` is `EV_ADD`, and the
//! remaining four bytes are `fflags == 0`. `0x20` is 32 bytes past the `udata`,
//! so the element is 48 bytes, which is `sizeof(struct kevent64_s)` and not the
//! 32-byte `struct kevent` the sibling `kevent` takes.
//!
//! **The six numbers that matter, therefore, are: `EVFILT_MACHPORT`, `EV_ADD`,
//! `fflags == 0`, `data == 0`, `udata == the caller's index`, and a null
//! timeout.** Everything below is in service of that one shape.
//!
//! ## What a delivered event says
//!
//! `<sys/event.h>` documents the answer, and it is not a count:
//!
//! > If no message receipt options were provided in the fflags field on setup,
//! > no message is received by this call. Instead, on output, the data field
//! > simply contains the name of the actual port detected with a message
//! > waiting.
//!
//! So `data` is a `mach_port_t`, and for a port set it is the *member* that has
//! the message rather than the set. Chromium reads only `udata` and takes the
//! lowest index it was given, which is what `WaitManyImpl` is documented to
//! return.
//!
//! ## What is refused, and why that is an answer
//!
//! Every other filter. `EVFILT_READ` and `EVFILT_WRITE` on a *Mach* port name
//! are not a thing - a Mach port is not a descriptor, and the filters that name
//! one are about a `vnode`, a timer or a process, none of which this runtime
//! models - and the eight `kevent64` call sites the framework has besides
//! [`WaitManyImpl`] belong to `base/files`'s watchers, which want `EVFILT_VNODE`
//! and a path that does not exist here.
//!
//! Also refused, by name and with a message: a `fflags` that asks for the
//! message to be *received during delivery* (the SDK says that form supports
//! port sets only, and it needs a caller-supplied receive buffer in `ext[0]`
//! and `ext[1]`), a non-zero `flags` argument, and a registration whose `ident`
//! is not a receive right or a port set in this task's space.
//!
//! The one thing that cannot be got right here and is not attempted: a
//! registration whose port is destroyed while a `kevent64` is waiting on it.
//! Nothing in this namespace announces a port's death, so rather than invent a
//! wakeup the registration is reported once as `EV_ERROR` with `data ==
//! EBADF` and then dropped. Reporting a port as *readable* when it is not is
//! the failure this file is written to avoid: Chromium would spin on a
//! `mach_msg` that can never return a message.

use std::collections::HashMap;
use std::ffi::c_int;
use std::sync::{Arc, Mutex, OnceLock};

use super::space::{self, Name};

/// `struct kevent64_s` from `<sys/event.h>`, in the field order it declares.
///
/// **Not packed, and not the 32-byte `struct kevent`.** The two differ in their
/// `data` and `udata` widths, in the trailing `ext[2]`, and therefore in their
/// size: 32 bytes against 48. A stride read off the wrong one puts every event
/// after the first at the wrong offset, which is silent corruption rather than
/// a failure, and the framework's own stride is 48 (`add rsi,0x30` in the loop
/// that zeroes the change list, `lea r15,[rax+rax*2]` for `count * 48`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Kevent64 {
    /// A `mach_port_t` for `EVFILT_MACHPORT`, a descriptor for the filters that
    /// name one. Stored as 64 bits because the structure says so; the guest
    /// writes a 32-bit name into the low half and leaves the high half zero.
    pub ident: u64,
    pub filter: i16,
    pub flags: u16,
    pub fflags: u32,
    pub data: i64,
    pub udata: u64,
    /// Filter-specific extensions. `EV_SET64`'s last two arguments, which the
    /// receive-during-delivery form of `EVFILT_MACHPORT` uses as a receive
    /// buffer and its length; zero for everything this serves.
    pub ext: [u64; 2],
}

const _: () = {
    assert!(size_of::<Kevent64>() == 48);
    assert!(std::mem::offset_of!(Kevent64, filter) == 8);
    assert!(std::mem::offset_of!(Kevent64, fflags) == 12);
    assert!(std::mem::offset_of!(Kevent64, data) == 16);
    assert!(std::mem::offset_of!(Kevent64, udata) == 24);
    assert!(std::mem::offset_of!(Kevent64, ext) == 32);
};

const EVFILT_READ: i16 = -1;
const EVFILT_WRITE: i16 = -2;
const EVFILT_AIO: i16 = -3;
const EVFILT_VNODE: i16 = -4;
const EVFILT_PROC: i16 = -5;
const EVFILT_SIGNAL: i16 = -6;
const EVFILT_TIMER: i16 = -7;
const EVFILT_MACHPORT: i16 = -8;
const EVFILT_FS: i16 = -9;
const EVFILT_USER: i16 = -10;
const EVFILT_VM: i16 = -12;
const EVFILT_EXCEPT: i16 = -15;

const EV_ADD: u16 = 0x0001;
const EV_DELETE: u16 = 0x0002;
const EV_ENABLE: u16 = 0x0004;
const EV_DISABLE: u16 = 0x0008;
const EV_ONESHOT: u16 = 0x0010;
const EV_CLEAR: u16 = 0x0020;
const EV_RECEIPT: u16 = 0x0040;
const EV_DISPATCH: u16 = 0x0080;
const EV_ERROR: u16 = 0x4000;

/// The action bits `EVFILT_MACHPORT` is served for.
///
/// `EV_CLEAR` is in this list and does nothing, and that is a decision rather
/// than an oversight: a port's event is level-triggered by construction, because
/// the message that fired it is still at the head of the queue, so there is no
/// state to clear. A filter that *consumes* its event - a timer, a user event -
/// is what `EV_CLEAR` exists for.
///
/// Everything outside the list is refused rather than ignored: the filter's own
/// `EV_FLAG0` and `EV_FLAG1` mean nothing to a Mach port, and `EV_SYSFLAGS` is
/// reserved by the kernel.
const SERVED: u16 =
    EV_ADD | EV_DELETE | EV_ENABLE | EV_DISABLE | EV_ONESHOT | EV_CLEAR | EV_RECEIPT | EV_DISPATCH;

/// What one `EV_ADD` recorded. There is only one of these per port name,
/// because there is only one filter: the key is the name, not the
/// `(ident, filter)` pair a general kqueue would use.
#[derive(Clone, Copy)]
struct Registration {
    ident: Name,
    flags: u16,
    fflags: u32,
    udata: u64,
    enabled: bool,
}

impl Registration {
    /// The event to hand back when this registration fires.
    fn fired(&self, source: Name) -> Kevent64 {
        Kevent64 {
            ident: self.ident as u64,
            filter: EVFILT_MACHPORT,
            // `EV_CLEAR` needs no handling and is not reported: a port's event
            // is level-triggered by construction, because the message that
            // fired it is still at the head of the queue. Only a filter that
            // consumes its event - a timer, a user event - has anything to
            // clear, and this is not one.
            flags: 0,
            // The caller's own `fflags`, and nothing else. The kernel adds a
            // `MACH_NOTE_MSG` bit here for a port that has a message waiting;
            // it is not in `<sys/event.h>`, `<sys/kern_event.h>`,
            // `<mach/port.h>` or `<mach/message.h>`, and a bit this runtime
            // cannot cite is a bit it does not invent.
            fflags: self.fflags,
            // "The name of the actual port detected with a message waiting."
            data: source as i64,
            udata: self.udata,
            ext: [0, 0],
        }
    }

    /// The event for a registration whose port is gone.
    ///
    /// `EV_ERROR`'s convention is that `data` carries an `errno`, so a caller
    /// that only reads the flags still learns something went wrong, and the
    /// `udata` is the only thing that says *which* registration.
    fn vanished(&self) -> Kevent64 {
        Kevent64 {
            flags: EV_ERROR,
            data: crate::errno::from_host(libc::EBADF) as i64,
            ..self.fired(self.ident)
        }
    }
}

#[derive(Default)]
struct Queue {
    registrations: HashMap<Name, Registration>,
}

fn queues() -> &'static Mutex<HashMap<c_int, Arc<Mutex<Queue>>>> {
    static QUEUES: OnceLock<Mutex<HashMap<c_int, Arc<Mutex<Queue>>>>> = OnceLock::new();
    QUEUES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether `queue` is a descriptor [`crate::kqueue::kqueue`] handed out.
///
/// Asked of [`crate::kqueue`] rather than of this module's own table, because
/// that is where the answer is kept: a descriptor that is not a kqueue has to
/// be `EBADF`, and the list of queues is that module's. `kevent` with nothing
/// to apply and nowhere to put an event does nothing at all except answer
/// exactly this question, so it is asked that way rather than by guessing what
/// kind of descriptor `queue` might be.
fn is_queue(queue: c_int) -> bool {
    // SAFETY: `nchanges` is zero and `nevents` is zero, so `kevent` reads
    // neither pointer and writes through none of them.
    unsafe {
        crate::kqueue::kevent(
            queue,
            std::ptr::null(),
            0,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        ) == 0
    }
}

/// The receive rights behind `name`, in the order a caller would look at them:
/// the port itself, or the members of a port set.
///
/// `None` for a name that is neither, which is what makes a registration of one
/// an `EINVAL` rather than a knote that can never fire.
fn ports_behind(name: Name) -> Option<Vec<Name>> {
    if space::receiver(name).is_some() {
        return Some(vec![name]);
    }
    let members = space::members(name)?;
    // A set with no usable member is a set whose event can never fire, which is
    // what Darwin does too, so it registers.
    Some(members)
}

/// The `poll` timeout in milliseconds, or `-1` for "wait forever".
///
/// Rounded **up**: a caller that asked to wait for half a millisecond asked to
/// wait, and a zero here would turn its wait into a poll - and for
/// `WaitManyImpl` a poll that finds nothing is `PCHECK(rv > 0)` failing, which
/// aborts the browser.
fn timeout_ms(timeout: *const libc::timespec) -> Result<c_int, c_int> {
    // SAFETY: null, or the caller's `struct timespec`, which is the same two
    // 64-bit words in this order on both platforms.
    let Some(timeout) = (unsafe { timeout.as_ref() }) else {
        return Ok(-1);
    };
    if timeout.tv_sec < 0 || !(0..1_000_000_000).contains(&timeout.tv_nsec) {
        return Err(crate::errno::from_host(libc::EINVAL));
    }
    let ms = (timeout.tv_sec as i64)
        .saturating_mul(1000)
        .saturating_add((timeout.tv_nsec as i64 + 999_999) / 1_000_000);
    Ok(ms.min(c_int::MAX as i64) as c_int)
}

/// Apply one change, answering with the `errno` it failed with.
///
/// The number is **Darwin's**: it reaches the guest as an `EV_ERROR` event's
/// `data`, which is a value rather than the `errno` slot, so nothing later
/// translates it.
fn apply(state: &mut Queue, change: &Kevent64) -> Result<(), i32> {
    if change.filter != EVFILT_MACHPORT {
        return Err(refused_filter(change.filter, change.ident));
    }
    if change.flags & !SERVED != 0 {
        return Err(refused_action(change.flags, change.ident));
    }
    if change.fflags != 0 {
        return Err(refused_receive_during_delivery(change.ident, change.fflags));
    }
    let name = name_of(change.ident);
    if change.flags & EV_DELETE != 0 {
        state
            .registrations
            .remove(&name)
            .map(|_| ())
            .ok_or_else(|| {
                // A port set's members are not the registration's key, so this
                // is `ENOENT` for the set's name as well as for a port's.
                announce_missing_registration(name, EV_DELETE);
                crate::errno::from_host(libc::ENOENT)
            })?;
        return Ok(());
    }
    if change.flags & EV_ADD != 0 {
        if ports_behind(name).is_none() {
            return Err(refused_ident(name));
        }
        state.registrations.insert(
            name,
            Registration {
                ident: name,
                flags: change.flags,
                fflags: change.fflags,
                udata: change.udata,
                enabled: change.flags & EV_DISABLE == 0,
            },
        );
        return Ok(());
    }
    let Some(registration) = state.registrations.get_mut(&name) else {
        announce_missing_registration(name, change.flags);
        return Err(crate::errno::from_host(libc::ENOENT));
    };
    if change.flags & EV_ENABLE != 0 {
        registration.enabled = true;
    }
    if change.flags & EV_DISABLE != 0 {
        registration.enabled = false;
    }
    Ok(())
}

/// The `mach_port_t` in an `ident`.
///
/// The structure's field is 64 bits wide and a Mach name is 32, and the guest
/// zero-initialises the high half - but a name with bit 32 set is not a name
/// this kernel could issue, so the high half is dropped rather than trusted.
/// Truncating also keeps a name from being confused with a descriptor, which is
/// what the same field means for the other filters.
fn name_of(ident: u64) -> Name {
    ident as u32
}

/// The filter's name, for a refusal. Anything not named here stays its number:
/// inventing a name for a filter this file has never heard of is how a
/// diagnostic starts lying.
fn filter_name(filter: i16) -> &'static str {
    match filter {
        EVFILT_READ => "EVFILT_READ",
        EVFILT_WRITE => "EVFILT_WRITE",
        EVFILT_AIO => "EVFILT_AIO",
        EVFILT_VNODE => "EVFILT_VNODE",
        EVFILT_PROC => "EVFILT_PROC",
        EVFILT_SIGNAL => "EVFILT_SIGNAL",
        EVFILT_TIMER => "EVFILT_TIMER",
        EVFILT_MACHPORT => "EVFILT_MACHPORT",
        EVFILT_FS => "EVFILT_FS",
        EVFILT_USER => "EVFILT_USER",
        EVFILT_VM => "EVFILT_VM",
        EVFILT_EXCEPT => "EVFILT_EXCEPT",
        _ => "a filter kevent64 does not define",
    }
}

/// A filter this file will not answer, named.
fn refused_filter(filter: i16, ident: u64) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused {} ({filter}) on ident {ident:#x}: a Mach port is not a \
             descriptor, and the filters that name one are about a vnode, a timer or a process, \
             none of which exist here. EVFILT_MACHPORT (-8) is the filter that answers \"is there \
             a message on this port\", and it is answered",
            filter_name(filter)
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// A `fflags` that asks `kevent64` to receive the message as it delivers the
/// event, which is the one documented form of `EVFILT_MACHPORT` this cannot do.
fn refused_receive_during_delivery(ident: u64, fflags: u32) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused fflags {fflags:#x} on ident {ident:#x}: the message \
             receipt options make kevent64 receive into a caller-supplied buffer named by \
             ext[0] and ext[1], and <sys/event.h> documents that form for port sets only. This \
             has no port sets with receive buffers behind them, so the event would report a \
             message that was never taken"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// A registration of something that is not a port with a queue.
fn refused_ident(name: Name) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused an EVFILT_MACHPORT registration of {name:#x}: it is not a \
             receive right or a port set in this task's space. EVFILT_MACHPORT's ident is a \
             mach_port_t, not a descriptor, so a value that names neither cannot be watched - and \
             answering anyway would be a knote that can never fire"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// A change naming a registration that is not there.
fn announce_missing_registration(name: Name, flags: u16) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64: ENOENT for {name:#x} with flags {flags:#x} - no such \
             registration on this queue. Darwin answers the same way, and it is the check that \
             catches a filter applied to a queue that was never told about it"
        );
    });
}

/// An action bit a Mach port filter has no meaning for, named.
fn refused_action(flags: u16, ident: u64) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64 refused flags {flags:#x} on ident {ident:#x}: the bits outside \
             {SERVED:#x} are the filter's own EV_FLAG0 and EV_FLAG1, which mean nothing to a Mach \
             port, and EV_SYSFLAGS, which the kernel reserves. Ignored rather than refused would \
             be a registration doing something other than what it says"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// Say once that a `kevent64` argument this file does not read was not zero.
fn refused_flags(flags: u32) -> i32 {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64: flags {flags:#x} asked for something this does not do. The only \
             caller in this tree passes 0, and a non-zero word is how a caller asks for a \
             kqueue workloop or for errors as events; both are answered with an event that is \
             neither"
        );
    });
    crate::errno::from_host(libc::EINVAL)
}

/// One descriptor to wait on, and the registration and port it belongs to.
struct Watch {
    fd: c_int,
    /// The port that actually has the message: the port itself, or the member
    /// of a port set. This is what `data` reports.
    source: Name,
    ident: Name,
    registration: Registration,
}

/// The descriptors to wait on, and the registrations whose port is gone.
fn watches(state: &mut Queue) -> (Vec<Watch>, Vec<Registration>) {
    // A registration whose port has been destroyed has no descriptor and no way
    // to be woken, so it is taken out here rather than waited on forever. There
    // is nothing to hook the destruction itself: `mach_port_destroy` is not this
    // module's.
    // The registration is *copied out* first, because reporting it to the
    // caller that was waiting for it is the whole point of taking it out.
    let mut vanished = Vec::new();
    state.registrations.retain(|name, registration| {
        let keep = space::receiver(*name).is_some() || space::members(*name).is_some();
        if !keep {
            vanished.push(*registration);
        }
        keep
    });
    let mut out = Vec::new();
    for registration in state.registrations.values() {
        if !registration.enabled {
            continue;
        }
        for source in ports_behind(registration.ident).unwrap_or_default() {
            // The `Receiver` is held for as long as the wait is, so the
            // descriptor cannot be closed underneath the `poll` below.
            let Some(port) = space::receiver(source) else {
                continue;
            };
            out.push(Watch {
                fd: port.fd(),
                source,
                ident: registration.ident,
                registration: *registration,
            });
        }
    }
    (out, vanished)
}

/// `int kevent64(int kq, const struct kevent64_s *changelist, int nchanges,
/// struct kevent64_s *eventlist, int nevents, unsigned int flags, const struct
/// timespec *timeout)`
///
/// The `flags` argument is a **value**, not a pointer, and the `timeout` is the
/// pointer: `<sys/event.h>` declares them in that order and the framework's own
/// call at `+0x5d9a9db` loads `xor r9d,r9d` for one and `mov QWORD PTR
/// [rsp],0x0` for the other, which is the only way to tell the two apart.
///
/// # Safety
///
/// `changelist` has `nchanges` readable events and `eventlist` has room for
/// `nevents` writable ones.
pub unsafe extern "C" fn kevent64(
    queue: c_int,
    changelist: *const Kevent64,
    nchanges: c_int,
    eventlist: *mut Kevent64,
    nevents: c_int,
    flags: u32,
    timeout: *const libc::timespec,
) -> c_int {
    if nchanges < 0 || nevents < 0 {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if (nchanges != 0 && changelist.is_null()) || (nevents != 0 && eventlist.is_null()) {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    if flags != 0 {
        crate::errno::set(crate::errno::to_host(refused_flags(flags)));
        return -1;
    }
    if !is_queue(queue) {
        crate::errno::set(libc::EBADF);
        return -1;
    }
    // `kqueue`'s own answer is not a filtered set, so a queue this module has
    // never been asked about is an empty one rather than a refusal. The
    // descriptor was just proved to be a kqueue, which is the only thing about
    // it that decides whether `kevent64` refuses.
    let state = queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(queue)
        .or_insert_with(|| Arc::new(Mutex::new(Queue::default())))
        .clone();
    let capacity = nevents as usize;
    let mut out: Vec<Kevent64> = Vec::with_capacity(capacity);

    // SAFETY: the caller states there are `nchanges` changes to read.
    let changes = match nchanges {
        0 => &[][..],
        n => unsafe { std::slice::from_raw_parts(changelist, n as usize) },
    };
    {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        for change in changes {
            let failure = apply(&mut state, change).err();
            // `EV_RECEIPT` asks for the outcome of *every* change rather than
            // for readiness, and it asks whether or not the change failed -
            // which is how a caller applies a batch and finds out which one the
            // kernel rejected.
            if change.flags & EV_RECEIPT != 0 {
                if out.len() < capacity {
                    out.push(Kevent64 {
                        flags: EV_ERROR,
                        data: failure.unwrap_or(0) as i64,
                        ..*change
                    });
                }
                continue;
            }
            if let Some(errno) = failure {
                crate::errno::set(crate::errno::to_host(errno));
                return -1;
            }
        }
    }
    if capacity == 0 {
        return 0;
    }
    if !out.is_empty() {
        // SAFETY: the caller states there is room for `nevents`.
        unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), eventlist, out.len()) };
        return out.len() as c_int;
    }

    let wait = match timeout_ms(timeout) {
        Ok(ms) => ms,
        Err(errno) => {
            crate::errno::set(crate::errno::to_host(errno));
            return -1;
        }
    };
    let (watched, mut vanished) = {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        watches(&mut state)
    };
    if watched.is_empty() && vanished.is_empty() {
        // Nothing is registered, so nothing can ever become ready.
        //
        // With a timeout that is simply Darwin's answer: nothing happened, and
        // `kevent64` says so by reporting no events. With no timeout at all
        // Darwin blocks here forever, and blocking is refused instead: a caller
        // that is told can say what it asked for, and a caller that is not told
        // gets a hang with nothing in the log to explain it.
        if wait < 0 {
            announce_empty_wait();
            crate::errno::set(libc::EINVAL);
            return -1;
        }
        // SAFETY: no descriptors, which `poll` allows, and the caller's own
        // timeout in milliseconds.
        if unsafe { libc::poll(std::ptr::null_mut(), 0, wait) } < 0 {
            crate::errno::set(crate::errno::get());
            return -1;
        }
        return 0;
    }

    let mut entries: Vec<libc::pollfd> = watched
        .iter()
        .map(|watch| libc::pollfd {
            fd: watch.fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    // SAFETY: `entries` is exactly the number of live `struct pollfd` the count
    // is derived from, and nothing else touches it.
    let ready = unsafe { libc::poll(entries.as_mut_ptr(), entries.len() as libc::nfds_t, wait) };
    if ready < 0 {
        let errno = crate::errno::get();
        // `EINTR` is reported rather than restarted. Darwin reports it, and
        // Chromium's `HANDLE_EINTR` around this call is written to see it and
        // try again with the same arguments; a restart here would wait longer
        // than the caller asked for.
        crate::errno::set(errno);
        return -1;
    }

    // A port that went away is reported once, and it comes before readiness so
    // that a caller draining a queue finds out about it rather than blocking on
    // it on the next turn.
    for registration in vanished.drain(..) {
        if out.len() < capacity {
            out.push(registration.vanished());
        }
    }
    if ready > 0 {
        let mut ready_events: Vec<(Name, Kevent64)> = Vec::new();
        for (entry, watch) in entries.iter().zip(&watched) {
            if entry.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0
            {
                continue;
            }
            // One event per registration, not per descriptor: a port set with
            // two ready members is one knote that fired, and `data` says which
            // member it was.
            if ready_events.iter().any(|(ident, _)| *ident == watch.ident) {
                continue;
            }
            ready_events.push((watch.ident, watch.registration.fired(watch.source)));
        }
        // Darwin's order among several ready knotes is the order they became
        // ready in, which a port queue does not record. Sorting by `udata`
        // instead makes two runs of the same program hand the caller the same
        // events in the same order, and puts the one `WaitManyImpl` cares about -
        // the lowest index - first even when `nevents` is too small for all of
        // them.
        ready_events.sort_by_key(|(ident, event)| (event.udata, *ident));
        ready_events.truncate(capacity - out.len());
        if !ready_events.is_empty() {
            let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
            for (ident, _) in &ready_events {
                // `EV_ONESHOT` deletes the registration once it has fired and
                // `EV_DISPATCH` disables it until it is enabled again. Both are
                // how a caller avoids being handed the same readiness every
                // turn, and neither may happen to a knote that was *not*
                // delivered - so this walks the delivered list, not the ready
                // one.
                let Some(registration) = state.registrations.get(ident).copied() else {
                    continue;
                };
                if registration.flags & EV_ONESHOT != 0 {
                    state.registrations.remove(ident);
                } else if registration.flags & EV_DISPATCH != 0
                    && let Some(registration) = state.registrations.get_mut(ident)
                {
                    registration.enabled = false;
                }
            }
        }
        out.extend(ready_events.into_iter().map(|(_, event)| event));
    }
    // SAFETY: the caller states there is room for `nevents`, and `out` never
    // grew past that.
    unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), eventlist, out.len()) };
    out.len() as c_int
}

/// Say once that a wait was asked for on a queue with nothing to watch.
fn announce_empty_wait() {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: kevent64: an untimed wait on a queue with nothing to watch - no \
             registration on this queue is a Mach port with a queue, so nothing can become ready \
             and no message can arrive to wake it. Darwin blocks here forever; EINVAL is refused \
             instead, because a caller that is told can say what it asked for and a caller that \
             is not told gets a hang with nothing in the log to explain it"
        );
    });
}

/// The registrations of a queue whose descriptor has been closed.
///
/// **This is the one thing this module cannot do for itself**, and the reason is
/// a table it does not own: a queue's state is keyed by its descriptor number,
/// and nothing here is told when a descriptor is closed - `close` is
/// [`crate::net::descriptor`]'s, and that is the one place in this crate where a
/// per-descriptor table is already thrown away. So a queue's registrations
/// outlive its descriptor until its ports stop existing ([`watches`] drops
/// those), and a descriptor number handed to a *new* `kqueue()` inherits
/// whatever the last queue at that number had registered.
///
/// The effect is bounded and it is not a hang and not a wrong readiness: the
/// inherited knotes are only the ones whose ports are still alive, and
/// `WaitManyImpl` adds its own with `EV_ADD`, which replaces the identical keys.
/// A fresh queue can therefore be handed an event for a port its caller never
/// named, which is a lower `udata` than the one that really fired. Chromium
/// re-checks the event it is handed, so the run survives it, and Darwin cannot
/// do it at all.
///
/// Calling this from `close` - the one line beside
/// `super::sockopt::forget(fd)` in [`crate::net::descriptor`] - is the whole fix,
/// and until it is there this is what the tests below use to start from a known
/// table. That is also why they are deterministic at all.
pub fn forget_queue(queue: c_int) {
    queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&queue);
}

// Published by [`super`]'s `exports!` block, beside every other `mach_` and
// `vm_` name this crate answers for, rather than from here: a second block would
// make a `MODULE` that nothing includes, which is how a name ends up implemented
// and unbound.

#[cfg(test)]
mod tests {
    use super::*;

    /// A zero timeout, which makes a call a collection rather than a wait.
    static NO_WAIT: libc::timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };

    /// A fresh queue, with whatever the last queue at this descriptor number had
    /// registered thrown away first.
    ///
    /// See [`forget_queue`]: a test that closes a queue and opens another gets
    /// the same descriptor number back, and without this every test in this
    /// module would inherit the previous one's knotes.
    fn queue() -> c_int {
        let queue = crate::kqueue::kqueue();
        forget_queue(queue);
        queue
    }

    fn receive_right() -> Name {
        let mut name = space::MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_RECEIVE, &mut name) };
        name
    }

    fn port_set() -> Name {
        let mut name = space::MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_PORT_SET, &mut name) };
        name
    }

    /// Chromium's `EV_SET64`: one event per waitable, the index in `udata`.
    fn add(port: Name, index: u64) -> Kevent64 {
        Kevent64 {
            ident: port as u64,
            filter: EVFILT_MACHPORT,
            flags: EV_ADD,
            fflags: 0,
            data: 0,
            udata: index,
            ext: [0, 0],
        }
    }

    /// An empty message to `port`, which is what a `WaitableEvent` signals with.
    fn signal(port: Name) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0u32.to_le_bytes()); // msgh_bits
        bytes.extend_from_slice(&24u32.to_le_bytes()); // msgh_size
        bytes.extend_from_slice(&port.to_le_bytes());
        bytes.extend_from_slice(&space::MACH_PORT_NULL.to_le_bytes());
        bytes.extend_from_slice(&space::MACH_PORT_NULL.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        // SAFETY: a whole message of the size in its header.
        let rc = unsafe {
            super::super::msg::mach_msg(
                bytes.as_mut_ptr().cast(),
                0x1, // MACH_SEND_MSG
                bytes.len() as u32,
                0,
                space::MACH_PORT_NULL,
                0,
                space::MACH_PORT_NULL,
            )
        };
        assert_eq!(rc, 0, "the signal reached the port");
    }

    /// One `kevent64` call, with the changelist the guest would pass.
    fn call(queue: c_int, changes: &[Kevent64], room: usize) -> (i32, Vec<Kevent64>) {
        let mut got = vec![Kevent64::default(); room];
        // SAFETY: `changes` is `changes.len()` events and `got` has `room` slots.
        let n = unsafe {
            kevent64(
                queue,
                changes.as_ptr(),
                changes.len() as c_int,
                got.as_mut_ptr(),
                room as c_int,
                0,
                &raw const NO_WAIT,
            )
        };
        (n, got)
    }

    /// The exact shape of `WaitManyImpl`: one queue, `EVFILT_MACHPORT` for
    /// every waitable, the index in `udata`, no timeout.
    #[test]
    fn a_port_with_a_message_reports_the_index_its_owner_gave_it() {
        let queue = queue();
        let first = receive_right();
        let second = receive_right();

        // Nothing signalled yet: a collection, not a wait, and the answer is
        // zero. Chromium's `PCHECK(rv > 0)` is not on this path because it
        // never asks.
        let (n, _) = call(queue, &[add(first, 0), add(second, 1)], 2);
        assert_eq!(n, 0, "neither port has a message");

        signal(second);
        let (n, got) = call(queue, &[], 2);
        assert_eq!(n, 1, "one port is ready");
        assert_eq!(got[0].ident, second as u64);
        assert_eq!(got[0].filter, EVFILT_MACHPORT);
        assert_eq!(
            got[0].udata, 1,
            "the caller's index, not the port's position"
        );
        assert_eq!(
            got[0].data, second as i64,
            "data is the port with the message"
        );
        assert_eq!(got[0].flags & EV_ERROR, 0);

        // ... and the same call again, because nothing was taken.
        let (n, _) = call(queue, &[], 2);
        assert_eq!(
            n, 1,
            "the message is still queued: this is a poll, not a receive"
        );

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(first);
            space::destroy(second);
        }
    }

    /// The lowest index is what `WaitManyImpl` returns, so two ready ports have
    /// to arrive in one call and both have to be there.
    #[test]
    fn two_ready_ports_are_one_call_and_both_are_reported() {
        let queue = queue();
        let first = receive_right();
        let second = receive_right();
        signal(first);
        signal(second);
        let (n, got) = call(queue, &[add(first, 0), add(second, 1)], 2);
        assert_eq!(n, 2);
        let udatas: Vec<u64> = got.iter().map(|event| event.udata).collect();
        assert_eq!(udatas, vec![0, 1]);

        // A room too small to hold both reports what fits, and drops the rest,
        // which is what Darwin does: `nevents` is a capacity, not a request.
        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1);
        assert_eq!(got[0].udata, 0);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(first);
            space::destroy(second);
        }
    }

    /// A port set is a filter too, and `data` is the member rather than the set.
    #[test]
    fn a_port_set_reports_the_member_that_has_the_message() {
        let queue = queue();
        let set = port_set();
        let member = receive_right();
        assert_eq!(
            space::mach_port_insert_member(space::TASK_SELF, member, set),
            0
        );

        let (n, _) = call(queue, &[add(set, 0)], 1);
        assert_eq!(n, 0);
        signal(member);
        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1);
        assert_eq!(got[0].ident, set as u64, "the event is the registration's");
        assert_eq!(got[0].data, member as i64, "and data names the member");

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(set);
            space::destroy(member);
        }
    }

    /// The contract `mach_port_peek` and `EVFILT_MACHPORT` share: **one event
    /// when there is a message, none when there is not.** A `kevent64_t` that
    /// claims a port is readable when its queue is empty is what sends Chromium
    /// into a `mach_msg` that never returns.
    #[test]
    fn a_reported_event_always_has_a_message_behind_it() {
        let queue = queue();
        let port = receive_right();
        call(queue, &[add(port, 0)], 1);

        for round in 0..3 {
            signal(port);
            let (n, _) = call(queue, &[], 1);
            assert_eq!(n, 1, "round {round}");

            // The same question `mach_port_peek` is asked, and it has to agree.
            let (mut seqno, mut size, mut id, mut trailer) = (0u32, 0u32, 0u32, [0u8; 8]);
            let mut offered = 8u32;
            // SAFETY: four live out-parameters and an eight-byte trailer buffer.
            let kr = unsafe {
                super::super::msg::mach_port_peek(
                    space::TASK_SELF,
                    port,
                    0,
                    &mut seqno,
                    &mut size,
                    &mut id,
                    trailer.as_mut_ptr(),
                    &mut offered,
                )
            };
            assert_eq!(kr, 0, "round {round}: kevent64 said ready and peek agrees");

            // Take it, and the event goes away.
            let mut buf = vec![0u8; 64];
            // SAFETY: 64 writable bytes.
            let rc = unsafe {
                super::super::msg::mach_msg(
                    buf.as_mut_ptr().cast(),
                    0x2 | 0x100, // MACH_RCV_MSG | MACH_RCV_TIMEOUT
                    0,
                    buf.len() as u32,
                    port,
                    1000,
                    space::MACH_PORT_NULL,
                )
            };
            assert_eq!(rc, 0, "round {round}: the message was received");
            let (n, _) = call(queue, &[], 1);
            assert_eq!(n, 0, "round {round}: and no longer reported");
        }

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// `EV_ONESHOT` is how a caller asks not to be handed the same readiness on
    /// every turn, and it has to be honoured or the caller loops.
    #[test]
    fn a_one_shot_registration_fires_once() {
        let queue = queue();
        let port = receive_right();
        let mut change = add(port, 0);
        change.flags = EV_ADD | EV_ONESHOT;
        assert_eq!(call(queue, &[change], 1).0, 0);

        signal(port);
        assert_eq!(call(queue, &[], 1).0, 1, "it fired");
        // The message is still there and the registration is not, so this is
        // zero - and `EV_DELETE` of it is now `ENOENT`.
        assert_eq!(call(queue, &[], 1).0, 0);
        let mut delete = add(port, 0);
        delete.flags = EV_DELETE;
        assert_eq!(call(queue, &[delete], 1).0, -1, "ENOENT");

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// `EV_CLEAR` on a port is accepted and changes nothing, because a port's
    /// event is level-triggered by construction: the message that fired it is
    /// still at the head of the queue. Only a filter that *consumes* its event
    /// - a timer, a user event - has anything to clear.
    #[test]
    fn a_cleared_port_registration_still_fires_every_turn() {
        let queue = queue();
        let port = receive_right();
        let mut change = add(port, 0);
        change.flags = EV_ADD | EV_CLEAR;
        assert_eq!(call(queue, &[change], 1).0, 0);

        signal(port);
        assert_eq!(call(queue, &[], 1).0, 1);
        assert_eq!(
            call(queue, &[], 1).0,
            1,
            "still there, because it is still there"
        );
        assert_eq!(call(queue, &[], 1).0, 1);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// `EV_DISABLE` takes a registration out of the set without deleting it, and
    /// `EV_ENABLE` puts it back.
    #[test]
    fn a_disabled_registration_is_not_reported_until_it_is_enabled() {
        let queue = queue();
        let port = receive_right();
        let mut disable = add(port, 0);
        disable.flags = EV_DISABLE;
        call(queue, &[add(port, 0), disable], 2);

        signal(port);
        assert_eq!(call(queue, &[], 1).0, 0, "a disabled knote does not fire");
        let mut enable = add(port, 0);
        enable.flags = EV_ENABLE;
        call(queue, &[enable], 1);
        assert_eq!(call(queue, &[], 1).0, 1, "and the message was still queued");

        // Enabling something that is not there is `ENOENT`, which is the check
        // that catches a filter applied to a queue that never heard of it.
        let mut orphan = add(port, 1);
        orphan.ident = 0x7fff_0000;
        orphan.flags = EV_ENABLE;
        assert_eq!(call(queue, &[orphan], 1).0, -1);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// A port that is destroyed while a queue still names it must not be waited
    /// on forever, and must not be reported as readable.
    #[test]
    fn a_registration_whose_port_is_destroyed_is_reported_once_and_dropped() {
        let queue = queue();
        let port = receive_right();
        assert_eq!(call(queue, &[add(port, 0)], 1).0, 0);
        space::destroy(port);

        let (n, got) = call(queue, &[], 1);
        assert_eq!(n, 1, "the wait was woken rather than left to hang");
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        assert_eq!(got[0].data as i32, crate::errno::from_host(libc::EBADF));
        assert_eq!(got[0].udata, 0, "so the caller can tell which one it was");
        // Dropped, not reported again.
        assert_eq!(
            call(queue, &[], 1).0,
            0,
            "and then there is nothing left to report"
        );

        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A filter this file will not answer is refused by number, and
    /// `EV_RECEIPT` is how the caller finds out which change it was.
    #[test]
    fn a_filter_that_is_not_a_mach_port_is_refused_by_number() {
        let queue = queue();
        let mut change = Kevent64 {
            ident: 1,
            filter: EVFILT_VNODE,
            flags: EV_ADD | EV_RECEIPT,
            ..Kevent64::default()
        };
        let (n, got) = call(queue, &[change], 1);
        assert_eq!(n, 1);
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        assert_eq!(got[0].data as i32, crate::errno::from_host(libc::EINVAL));

        // Without `EV_RECEIPT` the whole call fails.
        change.flags = EV_ADD;
        assert_eq!(call(queue, &[change], 1).0, -1);
        assert_eq!(
            crate::errno::get(),
            crate::errno::to_host(crate::errno::from_host(libc::EINVAL))
        );

        // And the filter this file does answer is still answerable afterwards.
        let port = receive_right();
        assert_eq!(call(queue, &[add(port, 0)], 1).0, 0);
        signal(port);
        assert_eq!(call(queue, &[], 1).0, 1);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::destroy(port);
        }
    }

    /// An `ident` that is not a port has no queue to watch, and registering it
    /// anyway would be a knote that can never fire.
    #[test]
    fn a_registration_of_something_that_is_not_a_port_is_refused() {
        let queue = queue();
        for ident in [
            space::TASK_SELF, // a special name
            0x7fff_0000u32,   // a name this namespace never issued
            u32::MAX,         // MACH_PORT_DEAD
        ] {
            let mut change = add(ident, 0);
            change.flags = EV_ADD | EV_RECEIPT;
            let (n, got) = call(queue, &[change], 1);
            assert_eq!(n, 1, "ident {ident:#x} reports its failure");
            assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
            assert_eq!(got[0].data as i32, crate::errno::from_host(libc::EINVAL));
        }
        // A send right is a name, and not a receive right.
        let mut receive = space::MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_RECEIVE, &mut receive) };
        let (mut send, mut kind) = (space::MACH_PORT_NULL, 0);
        // SAFETY: two live locals for the out-parameters.
        unsafe {
            space::mach_port_extract_right(
                space::TASK_SELF,
                receive,
                super::super::msg::MAKE_SEND,
                &mut send,
                &mut kind,
            )
        };
        let mut change = add(send, 0);
        change.flags = EV_ADD | EV_RECEIPT;
        let (n, got) = call(queue, &[change], 1);
        assert_eq!(n, 1);
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);

        // SAFETY: this test's own queue descriptor.
        unsafe {
            libc::close(queue);
            space::mach_port_deallocate(space::TASK_SELF, send);
            space::destroy(receive);
        }
    }

    /// A descriptor that is not a queue is `EBADF`, and that answer comes from
    /// `kqueue`'s own table rather than from guessing at the descriptor.
    #[test]
    fn a_descriptor_that_is_not_a_queue_is_refused() {
        let mut got = vec![Kevent64::default(); 1];
        // SAFETY: standard input is open and is not a kqueue.
        let n = unsafe {
            kevent64(
                0,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EBADF);
    }

    /// The seventh argument is a `timespec *` and the sixth is a value, and a
    /// value this file does not read is refused rather than ignored.
    #[test]
    fn a_flags_word_this_does_not_read_is_refused() {
        let queue = queue();
        let mut got = vec![Kevent64::default(); 1];
        // SAFETY: no changes and one event slot.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0x4,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A `timespec` with an out-of-range `tv_nsec` is `EINVAL` rather than a
    /// wait of a length nobody asked for.
    #[test]
    fn an_impossible_timeout_is_refused() {
        let queue = queue();
        let mut got = vec![Kevent64::default(); 1];
        let bad = libc::timespec {
            tv_sec: 0,
            tv_nsec: 2_000_000_000,
        };
        // SAFETY: no changes and one event slot.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0,
                &raw const bad,
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);
        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A wait that cannot ever end is refused, and named, while the same wait
    /// with a timeout in it is Darwin's own answer: no events.
    #[test]
    fn an_untimed_wait_with_nothing_registered_is_refused_rather_than_hung() {
        let queue = queue();
        let mut got = vec![Kevent64::default(); 1];
        // SAFETY: no changes, one event slot and a null timeout.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EINVAL);

        // With a timeout it is a collection that found nothing, which is what
        // Darwin reports and what a caller polling for a change can act on.
        // SAFETY: no changes, one event slot and a zero timeout.
        let n = unsafe {
            kevent64(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                0,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 0);
        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// The word the framework stores, read back the way the structure declares
    /// it. If the packing of `kevent64_s` is wrong every one of the tests above
    /// is testing a different function.
    #[test]
    fn the_event_is_48_bytes_with_its_fields_where_the_header_says() {
        assert_eq!(size_of::<Kevent64>(), 48);
        // `mov QWORD PTR [rsi+0x8],0x1fff8` - one store, three fields.
        let mut bytes = vec![0u8; 48];
        bytes[8..16].copy_from_slice(&0x0001_fff8u64.to_le_bytes());
        let mut event = Kevent64::default();
        // SAFETY: 48 bytes, which is the size of the value being read into it.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (&raw mut event).cast::<u8>(),
                size_of::<Kevent64>(),
            )
        };
        assert_eq!(event.filter, EVFILT_MACHPORT);
        assert_eq!(event.flags, EV_ADD);
        assert_eq!(event.fflags, 0);
    }
}
