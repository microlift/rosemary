#![cfg(not(target_os = "macos"))]
//! `kqueue` and `kevent` - the 32-bit `struct kevent` path.
//!
//! ## This is the `kevent` the framework's `EVFILT_PROC` registrations reach
//!
//! **`_kevent`, not `_kevent64`.** All three of the framework's `EVFILT_PROC`
//! registrations call the stub at `0xcca0e16`, which is `_kevent`;
//! `_kevent64` is the stub at `0xcca1728` and is served by
//! [`super::mach::kevent64`]. The eight-byte pattern
//! `0x8000_0000_0001_fffb` - `EVFILT_PROC | EV_ADD`, `fflags NOTE_EXIT` -
//! occurs exactly three times in the 214 MB image, at `0x5afcf4b`, `0x5d8cb12`
//! and `0x5daa1b4`, and all three are these eight instructions:
//!
//! ```text
//!  5afcf45  mov    QWORD PTR [rbp-0x80],rbx  ; ident = a pid
//!  5afcf49  movabs rax,0x800000000001fffb    ; EVFILT_PROC | EV_ADD, fflags NOTE_EXIT
//!  5afcf53  mov    QWORD PTR [rbp-0x78],rax
//!  5afcf5a  movups XMMWORD PTR [rbp-0x70],x0 ; data = 0, udata = 0
//!  5afcf72  call   0xcca0e16                ; _kevent
//! ```
//!
//! So this file used to be where the filter died: a `matches!` that allowed
//! `EVFILT_READ`, `EVFILT_WRITE` and `EVFILT_USER` and nothing else, returning a
//! bare `EINVAL`. The guest's `WaitForSingleNonChildProcess` and
//! `RelauncherSynchronizeWithParent` read `errno == ESRCH` as "the process is
//! dead" and log everything else - and `EINVAL` is a log line in a *release*
//! build's discarded `DLOG`, so **the run showed nothing at all** and the
//! browser simply never learned that a process had died.
//!
//! The filter is answered here now, and it is answered in the *readiness*
//! machinery of this file rather than beside it: [`apply_proc`] takes the
//! decision from [`super::mach::kevent64::proc_change`] - the XNU citations for
//! it are there - opens the `pidfd` with
//! [`super::mach::kevent64::pidfd_open`], and holds a
//! [`super::mach::kevent64::Proc`] in the same `HashMap<(ident, filter)>` as
//! every other registration, so that `EV_DELETE`, `EV_DISABLE` and a one-shot
//! delivery are one code path rather than two that have to agree.
//!
//! ## The one thing that is different about this filter, and where it bites
//!
//! **An `EVFILT_PROC` `ident` is a pid, not a descriptor.** Everything else in
//! this file is keyed by descriptor number, in three places that all had to be
//! taught the difference: [`Registration::watches`] decides which registration a
//! ready descriptor belongs to, [`descriptor_of`] says which descriptor a
//! registration waits on, and [`interest_for`] folds a descriptor's interest.
//! Getting that wrong does not fail - it *answers*: a pid that happened to equal
//! some other registration's descriptor number would be reported as that
//! registration's readiness, on a descriptor nobody named. The test
//! `a_pid_is_not_a_descriptor` pins the three of them directly.
//!
//! ## What a delivered event says
//!
//! `filter`, `fflags` and `ident` are the caller's own values with the kernel's
//! one substitution - `fflags` is the note that *fired*, which for this filter is
//! always `NOTE_EXIT`. `flags` is the caller's own word plus the three bits XNU
//! adds, and `data` is zero. All of that is derived once, in
//! [`super::mach::kevent64::Proc::exited`], from the XNU source; see
//! `PROC_DELIVERED_FLAGS` there for the `kn_flags` chain that puts the kernel's
//! bits - and the caller's `EV_ADD` - in the delivered word.
//!
//! ## A correction this file used to carry, and one it used to get wrong
//!
//! [`refused_filter`] used to close with: *"of every filter this file refuses,
//! `EVFILT_PROC` was the only one that any of the framework's `kevent` callers
//! actually waited on ... The rest are refused by name and cost nothing, because
//! no caller blocks on them."*
//!
//! **That is false, and the disassembly says so.** `EVFILT_VNODE` is registered
//! **and waited on** at four of this file's call sites - `+0x5d9e968` and
//! `+0x5d9f56e` in `base::FilePathWatcherKQueue`, `+0x3a9ab38` and `+0x3a9b73e`
//! in `content::file_system_access::FilePathWatcherKQueue` - each of them
//! `kevent(kq, changes, n, out, n, NULL)`: a null timeout and room for events,
//! which is a wait. The registration word is `movabs rax,0x6f0061fffc` at
//! `+0x5d9e7e8`, `+0x5d9ecfe`, `+0x5d9edbb`, `+0x3a9a9ba`, `+0x3a9aed0` and
//! `+0x3a9af8d`.
//!
//! So the refused set was **not** free of waits, and the sentence was wrong about
//! that. `EVFILT_VNODE` was the one refusal in the whole kqueue surface that was
//! **both reached and blocking**: runs 219, 220, 221, 222 and 223 each refused it
//! four times, in two shapes, both of which had room for events. (Runs 215, 216
//! and 218 refused it **zero** times, with the same binary and no code change -
//! the run's own start-up order decides, which is the nondeterministic wall the
//! project brief describes.) The guest's handling of the `EINVAL` was to fall
//! through rather than retry, so it was not a hang; the cost was a file watcher
//! that never fired, and `FilePathWatcherKQueue::Watch` returning **false**.
//!
//! **It is answered now**, on a real `inotify` watch - see
//! [`super::mach::kevent64`]'s module note for the note-to-event mapping, the
//! measured `inotify` rows behind it, and which three notes are refused by name.
//!
//! ## `EV_RECEIPT`: a synthetic receipt satisfies the guest, and that is settled
//!
//! Every `EVFILT_VNODE` registration the framework makes carries `EV_RECEIPT`, and
//! `FilePathWatcherKQueue::AreKeventValuesValid` is the function that judges it.
//! **The question was whether a receipt this runtime synthesizes would be
//! accepted. It is, and the answer is two instructions, measured rather than
//! inferred** - `AreKeventValuesValid` is not inlined, it is a real function at
//! `Chromium Framework+0x5d9e4c0`, called from `OnKQueueReadable` at `+0x5d9f097`:
//!
//! ```text
//!  5d9e4c0  test  edx,edx                      ; count < 0  -> false
//!  5d9e505  test  BYTE PTR [rbx+r15*1+0xb],0x40 ; flags & EV_ERROR
//!  5d9e50b  je   0x5d9e4f2
//!  5d9e510  cmp  QWORD PTR [r15+0x10],0x0      ; data != 0 ?
//!  5d9e515  je   0x5d9e4f2                    ; data == 0 -> STILL VALID
//! ```
//!
//! The whole of its validity test is **bit 14 of `flags` and whether `data` is
//! zero**. There is no check on `fflags`, no check on `udata`, and no check on
//! where the event came from - so a receipt whose `data` is `0` is accepted and a
//! receipt whose `data` is an `errno` is rejected, whatever produced it. The
//! compiler folded `flags & EV_ERROR` into the *high byte* of `flags` (`+0x0b`),
//! which is why a 16-bit `test ...,0x4000` scan of `__TEXT,__text` finds nothing
//! and reports a false zero.
//!
//! Two independent facts settle it, and both are measurements:
//!
//! * **The positive control.** A byte scan for that 8-bit form, at every byte
//!   offset within `__TEXT,__text`'s declared size, finds **exactly two** hits in
//!   214 MB: `0x5d9e506` and its `__FILE__`-bracketed twin at `0x3a9a6d6`, with
//!   the same instruction sequence and the same relative branch displacements.
//!   So the scan is not blind, and there is no third copy of this check.
//! * **The behaviour is already live.** Before any of this, a refused
//!   `EVFILT_VNODE` produced a receipt with `data == EINVAL`, the guest rejected
//!   it at `+0x5d9f09c`, `Watch()` returned `false`, and the run carried on. So
//!   the receipt path was reached, read and acted on on every one of those four
//!   refusals per run. The only thing that changes is the one bit: `data` becomes
//!   `0`, and `Watch()` returns `true`.
//!
//! A receipt is not fabricated: it is the caller's own change echoed back with
//! `flags = EV_ERROR` and `data` set to the outcome of a change that was
//! actually carried out. `data == 0` means "this change succeeded", and that is
//! a true statement.
//!
//! ## Which filters the guest passes, and which of them it waits on
//!
//! The whole-bundle census is in [`super::mach::kevent64`]'s module note, because
//! that is where it was measured. What belongs here is the part about *this*
//! file's twelve `_kevent` call sites:
//!
//! | call site | change list | wait? | function |
//! |---|---|:--:|---|
//! | `+0x3a9ab38` | `EVFILT_VNODE\|EV_ADD\|EV_CLEAR\|EV_RECEIPT`, `fflags 0x6f` | **yes** | `content::...::FilePathWatcherKQueue` |
//! | `+0x3a9b73e` | the same | **yes** | the same, a second path |
//! | `+0x3a9b22d` | none | collect, zero timeout | the same, `OnKQueueReadable` |
//! | `+0x5d9e968` | the same `EVFILT_VNODE` word | **yes** | `base::FilePathWatcherKQueue` |
//! | `+0x5d9f56e` | the same | **yes** | the same, a second path |
//! | `+0x5d9f05d` | none | collect, zero timeout | the same, `OnKQueueReadable` |
//! | `+0x5afcf72` | `EVFILT_PROC\|EV_ADD`, `NOTE_EXIT` | no | `RelauncherSynchronizeWithParent` |
//! | `+0x5afd036` | none | **yes** | the relauncher waiting on its own queue |
//! | `+0x5d8cb53` | `EVFILT_PROC\|EV_ADD`, `NOTE_EXIT` | no | `base::WaitForSingleNonChildProcess` |
//! | `+0x5d8ce63` | none | **yes**, with a deadline | `Process::WaitForExitWithTimeout` |
//! | `+0x5daa1e2` | `EVFILT_PROC\|EV_ADD`, `NOTE_EXIT` | no | `base::WaitForChildToDie` |
//! | `+0x5daa304` | none | **yes**, with a deadline | the same function's wait |
//!
//! ## `EVFILT_READ` on a descriptor, and `EVFILT_READ` on a Mach port
//!
//! These are different questions and the difference is worth stating, because
//! conflating them is what made the `kevent64` refusals look alarming.
//!
//! **This file answers the descriptor form**, on a real descriptor, with
//! `poll`/`epoll` underneath: readiness, a byte count from `FIONREAD`, `EV_EOF`
//! on a hangup, and `EV_ONESHOT`/`EV_DISPATCH` honoured. Nothing in the shipped
//! guest reaches it - there is **no `EVFILT_READ|EV_ADD` anywhere in the bundle
//! that reaches a `kevent` call** - and that is worth knowing precisely, because
//! it means the readiness machinery here is currently unexercised rather than
//! wrong.
//!
//! **The other form is a different call and a different refusal.** It arrives
//! through `_kevent64`, at `+0x5da4eec` for a deregistration and at `+0x5da7c0d`
//! for a one-shot registration, and it is refused there. **The "26 `EVFILT_READ`
//! refusals" were 26 *runs*, not 26 occurrences**: the site is `+0x5da7c0d` and
//! `nevents == 0` there (`xor %ecx,%ecx; xor %r8d,%r8d` at `+0x5da7c05`), so it
//! **cannot block**. And the `ident` at that site is `0x23` - an **open
//! descriptor**, not a Mach port - which is what
//! [`super::mach::kevent64::describe_ident`] is for; a message that says "a Mach
//! port is not a descriptor" about an open descriptor asserts the opposite of
//! the truth.
//!
//! ## The runtime census, and what it says
//!
//! `ROSETTA3_TRACE_KEVENT=1` (off by default) prints one line per distinct
//! **shape** of `kevent` call a run reaches, with a running count, and says on
//! every line whether the call had room for an event - which is what decides
//! whether anything refused in it could have hung a thread. Over runs 215, 216
//! and 218:
//!
//! | shape | count | verdict |
//! |---|--:|---|
//! | `EVFILT_VNODE\|EV_ADD\|EV_CLEAR\|EV_RECEIPT`, `fflags 0x6f`, 1 change, 1 event slot | 1 | **can block**, and was refused |
//! | the same, 3 changes, 3 event slots | 2 | **can block**, and was refused |
//! | no change list, 0 event slots | 32 | collection, cannot block |
//! | no change list, 4 event slots, `KEVENT_FLAG_IMMEDIATE` | 16 | a **wait**, on registrations that *were* accepted |
//!
//! **No `EVFILT_READ`, `EVFILT_WRITE` or `EVFILT_USER` shape appears at all.** So
//! the readiness path in this file - the `poll`/`epoll` reconstruction, the
//! `FIONREAD` byte count, `EV_ONESHOT` and `EV_DISPATCH` - is correct but
//! unexercised by the shipped guest's `kevent` path. That is worth knowing
//! precisely: it means the code here is not what is failing, and it also means
//! its correctness rests on this crate's tests rather than on the milestone.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{c_int, c_void};
use std::sync::{Arc, Mutex};

use static_init::dynamic;

/// `struct kevent`, which the guest both fills in and reads back.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Kevent {
    pub ident: usize,
    pub filter: i16,
    pub flags: u16,
    pub fflags: u32,
    pub data: isize,
    pub udata: usize,
}

const _: () = {
    assert!(size_of::<Kevent>() == 32);
    assert!(std::mem::offset_of!(Kevent, filter) == 8);
    assert!(std::mem::offset_of!(Kevent, data) == 16);
    assert!(std::mem::offset_of!(Kevent, udata) == 24);
};

const EVFILT_READ: i16 = -1;
const EVFILT_WRITE: i16 = -2;
const EVFILT_USER: i16 = -10;
/// `EVFILT_PROC`, taken from [`super::mach::kevent64`] rather than written
/// here: the number is `-5` in `<sys/event.h>` and it exists exactly once in
/// this crate, next to the XNU citations for what the filter means.
const EVFILT_PROC: i16 = super::mach::kevent64::EVFILT_PROC;
/// `EVFILT_VNODE`, taken from [`super::mach::kevent64`] rather than written
/// here, for the same reason `EVFILT_PROC` is: the number is `-4` in
/// `<sys/event.h>` and it exists exactly once in this crate, next to the XNU
/// citations for what the filter means and the `inotify` measurement for what
/// this host can say about each of its notes.
const EVFILT_VNODE: i16 = super::mach::kevent64::EVFILT_VNODE;

const EV_ADD: u16 = 0x0001;
const EV_DELETE: u16 = 0x0002;
const EV_ENABLE: u16 = 0x0004;
const EV_DISABLE: u16 = 0x0008;
const EV_ONESHOT: u16 = 0x0010;
const EV_CLEAR: u16 = 0x0020;
const EV_RECEIPT: u16 = 0x0040;
const EV_DISPATCH: u16 = 0x0080;
const EV_ERROR: u16 = 0x4000;
const EV_EOF: u16 = 0x8000;

/// `NOTE_TRIGGER`, the one `fflags` bit `EVFILT_USER` acts on.
const NOTE_TRIGGER: u32 = 0x0100_0000;

/// What one `EV_ADD` recorded.
///
/// **`Clone` and not `Copy`, because an `EVFILT_PROC` registration holds a
/// `pidfd` behind an `Arc`.** That is the whole reason the filter lives in
/// *this* table rather than in a parallel one keyed by pid: an `EV_DELETE`, an
/// `EV_DISABLE` and a one-shot delivery are all "drop this `Arc`", and a second
/// table would be a second place for that to be true or not to be true. The
/// `Arc` is what keeps the descriptor alive across a wait on another thread - a
/// `close` underneath a `poll` is a use-after-close - so the queue's copy and
/// the wait's copy are two references to one descriptor and the last one out
/// closes it.
#[derive(Clone)]
struct Registration {
    ident: usize,
    filter: i16,
    flags: u16,
    fflags: u32,
    udata: usize,
    enabled: bool,
    /// `Some` for an `EVFILT_PROC` registration: the `pidfd` the process is
    /// watched through, which is a **descriptor**, while `ident` above is a
    /// **pid**. Nothing here may treat the two as one number space - see
    /// [`Registration::watches`].
    proc: Option<Arc<super::mach::kevent64::Proc>>,
    /// `Some` for an `EVFILT_VNODE` registration: the `inotify` watch on the
    /// object `ident` names, and the notes it has accumulated.
    ///
    /// **As with `proc` above, the thing that becomes readable is not `ident`.**
    /// An `EVFILT_VNODE`'s `ident` *is* a descriptor, but the descriptor cannot
    /// be waited on; what becomes readable is the kqueue's own `inotify`
    /// instance, and that is what [`Vnode::inotify_fd`] reports. See
    /// [`Registration::watches`].
    vnode: Option<Arc<super::mach::kevent64::Vnode>>,
}

impl Registration {
    /// The event to hand back when this registration fires.
    ///
    /// `NOTE_TRIGGER` is cleared: it is the caller's *request*, and a caller
    /// that read it back in a delivered event would see its own trigger as a
    /// property of the event.
    fn fired(&self) -> Kevent {
        // An `EVFILT_PROC` event is not built out of this struct's own fields at
        // all: `fflags` is the note that actually fired rather than the caller's
        // word, and `flags` is the caller's word with the kernel's three
        // additions. Both come from `super::mach::kevent64`, where the XNU
        // citations for them are, so that there is one derivation of them in the
        // crate rather than two that can drift.
        if let Some(proc) = &self.proc {
            let exited = proc.exited();
            return Kevent {
                ident: exited.ident as usize,
                filter: EVFILT_PROC,
                flags: exited.flags,
                fflags: exited.fflags,
                data: exited.data as isize,
                udata: self.udata,
            };
        }
        Kevent {
            ident: self.ident,
            filter: self.filter,
            flags: 0,
            fflags: self.fflags & !NOTE_TRIGGER,
            data: 0,
            udata: self.udata,
        }
    }

    /// The event to hand back for an `EVFILT_VNODE` registration, if one is
    /// waiting.
    ///
    /// **`None` when nothing has fired, and that is not the same as an event with
    /// an empty `fflags`.** A `kevent` that reported an event with no note in it
    /// would be a readiness the caller cannot act on and cannot distinguish from
    /// a real one, and `FilePathWatcherKQueue::OnKQueueReadable` is a loop over
    /// whatever `count` says it got - so the count is the only thing keeping a
    /// spurious entry out of it.
    ///
    /// **`fflags` is the note word that was actually observed**, not the caller's
    /// interest: the `&self.fflags` in [`Registration::fired`] is right for a
    /// readiness filter, whose `fflags` is a low-water mark rather than a set of
    /// results, and it would be wrong here, where a delivered `fflags` is the set
    /// of notes that fired. XNU's shape is the same - `knote_fill_kevent` copies
    /// `kn_fflags` while `kn_sfflags` is what stays behind - and
    /// `filt_vnode_common` only ORs a hint in under `if (kn->kn_sfflags & hint)`,
    /// so a delivered bit is always one the caller asked for.
    ///
    /// **`data` is 0**, because that is what `filt_vnprocess` passes:
    /// `knote_fill_kevent(kn, kev, data)` with the `int64_t data = 0` that
    /// `filt_vnode_common` declares, and that only its `EVFILT_READ` and
    /// `EVFILT_WRITE` arms ever change. And `ident` and `udata` are the caller's
    /// own, which the framework matches on - both `AreKeventValuesValid` and
    /// `OnKQueueReadable` compare the delivered `ident` against the one it
    /// registered.
    fn vnode_fired(&self) -> Option<Kevent> {
        let watch = self.vnode.as_ref()?;
        let clear = self.flags & EV_CLEAR != 0;
        let fflags = watch.take_pending(clear);
        if fflags == 0 {
            return None;
        }
        super::mach::kevent64::announce_vnode_delivered(watch, fflags);
        Some(Kevent {
            ident: watch.ident(),
            filter: EVFILT_VNODE,
            flags: watch.delivered_flags(fflags),
            fflags,
            data: 0,
            udata: watch.udata(),
        })
    }

    /// Is this registration waiting on the descriptor `fd`?
    ///
    /// **A pid is not a descriptor, and this is where that is enforced.** Every
    /// other filter's `ident` names a descriptor, so "is this registration
    /// waiting on `fd`" is an equality test against `ident`. An `EVFILT_PROC`'s
    /// `ident` names a **process**, and the descriptor it waits on is the
    /// `pidfd` inside the registration. Without the split, a pid that happened
    /// to equal some *other* registration's descriptor number would be reported
    /// as that registration's readiness: a readiness nobody asked for, on a
    /// descriptor that was never named, which is exactly the silent wrong answer
    /// this module is written to avoid.
    fn watches(&self, fd: c_int) -> bool {
        match self.filter {
            EVFILT_PROC => self.proc.as_ref().is_some_and(|proc| proc.raw_fd() == fd),
            // **The `inotify` instance, never the `ident`.** An `EVFILT_VNODE`'s
            // `ident` *is* a descriptor - the one the caller opened with
            // `O_EVTONLY` - but it cannot be waited on: `inotify_add_watch` takes
            // a pathname rather than a descriptor (measured: it answers `EFAULT`
            // for an `O_PATH` descriptor, because the second argument is a
            // `const char *`), so the watch is on a path reached from
            // `/proc/self/fd`. What becomes readable is the queue's own `inotify`.
            //
            // Matching on `ident` instead would put an `O_PATH` descriptor into
            // an `epoll` set, which this host answers `EPERM`, and would report
            // the readiness of a descriptor nobody is waiting on.
            EVFILT_VNODE => self.vnode.as_ref().is_some_and(|watch| watch.inotify_fd() == fd),
            _ => self.ident == fd as usize,
        }
    }
}

struct Queue<B> {
    registrations: HashMap<(usize, i16), Registration>,
    /// Triggered `EVFILT_USER` idents, in the order they were triggered, and
    /// the set form of the same thing - a second trigger before the first is
    /// delivered is one event, not two.
    triggered: VecDeque<usize>,
    triggered_set: HashSet<usize>,
    /// This queue's own `inotify` instance, created by the first
    /// `EVFILT_VNODE` registration on it and released with the last one. It is
    /// never shared with another queue, and that is a decision with a reason:
    /// see [`apply_vnode`].
    ///
    /// **It is the single authority for which instance this queue uses.** Every
    /// registration reaches the same `Arc` through its own [`Vnode`], so there is
    /// one place that can be wrong about it, and [`kevent`] drains this one - the
    /// instance whose events the registrations on this queue are waiting for.
    inotify: Option<Arc<super::mach::kevent64::Inotify>>,
    backend: Arc<B>,
}

impl<B> Queue<B> {
    fn new(backend: B) -> Self {
        Self {
            registrations: HashMap::new(),
            triggered: VecDeque::new(),
            triggered_set: HashSet::new(),
            inotify: None,
            backend: Arc::new(backend),
        }
    }
}

/// A ready descriptor reported by one concrete host readiness mechanism.
#[derive(Clone, Copy)]
struct Ready {
    fd: c_int,
    revents: i16,
    wake: bool,
}

/// The host-specific part of a kqueue. `Queue<B>` is monomorphized with the
/// selected backend; this is deliberately not a trait object or a run-time
/// host capability choice.
trait ReadinessBackend: Sized {
    fn new() -> Result<(Self, c_int), c_int>;
    fn wake(&self);
    fn drain_wake(&self);
    fn update(&self, fd: c_int, before: i16, after: i16) -> Result<(), c_int>;
    fn wait(
        &self,
        queue: c_int,
        registrations: &HashMap<(usize, i16), Registration>,
        capacity: usize,
        timeout: c_int,
    ) -> Result<Vec<Ready>, c_int>;
}

/// The portable reconstruction of a readiness set. It remains the complete
/// implementation outside Linux.
#[cfg(not(target_os = "linux"))]
struct PollBackend {
    wake: c_int,
}

#[cfg(not(target_os = "linux"))]
impl ReadinessBackend for PollBackend {
    fn new() -> Result<(Self, c_int), c_int> {
        let wake = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if wake < 0 {
            Err(crate::errno::get())
        } else {
            Ok((Self { wake }, wake))
        }
    }

    fn wake(&self) {
        wake_fd(self.wake)
    }
    fn drain_wake(&self) {
        drain_fd(self.wake)
    }
    fn update(&self, _: c_int, _: i16, _: i16) -> Result<(), c_int> {
        Ok(())
    }

    fn wait(
        &self,
        queue: c_int,
        registrations: &HashMap<(usize, i16), Registration>,
        _: usize,
        timeout: c_int,
    ) -> Result<Vec<Ready>, c_int> {
        let mut watched = poll_set(queue, registrations);
        let ready =
            unsafe { libc::poll(watched.as_mut_ptr(), watched.len() as libc::nfds_t, timeout) };
        if ready < 0 {
            return Err(crate::errno::get());
        }
        Ok(watched
            .into_iter()
            .filter(|entry| entry.revents != 0)
            .map(|entry| Ready {
                fd: entry.fd,
                revents: entry.revents,
                wake: entry.fd == queue,
            })
            .collect())
    }
}

#[cfg(target_os = "linux")]
struct EpollBackend {
    epoll: c_int,
    wake: c_int,
    poll_only: Mutex<HashSet<c_int>>,
}

#[cfg(target_os = "linux")]
impl Drop for EpollBackend {
    fn drop(&mut self) {
        // This is the private wakeup descriptor, never the guest-visible
        // kqueue descriptor (which the guest owns and may already have closed).
        unsafe { libc::close(self.wake) };
    }
}

#[cfg(target_os = "linux")]
impl EpollBackend {
    const WAKE_TOKEN: u64 = u64::MAX;

    fn event(fd: c_int, interest: i16) -> libc::epoll_event {
        libc::epoll_event {
            events: ((interest & libc::POLLIN != 0) as u32 * libc::EPOLLIN as u32)
                | ((interest & libc::POLLOUT != 0) as u32 * libc::EPOLLOUT as u32),
            u64: fd as u64,
        }
    }

    fn ctl(epoll: c_int, op: c_int, fd: c_int, interest: i16) -> Result<(), c_int> {
        let mut event = Self::event(fd, interest);
        if unsafe { libc::epoll_ctl(epoll, op, fd, &raw mut event) } < 0 {
            Err(crate::errno::get())
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
impl ReadinessBackend for EpollBackend {
    fn new() -> Result<(Self, c_int), c_int> {
        let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epoll < 0 {
            return Err(crate::errno::get());
        }
        let wake = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if wake < 0 {
            unsafe { libc::close(epoll) };
            return Err(crate::errno::get());
        }
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: Self::WAKE_TOKEN,
        };
        if unsafe { libc::epoll_ctl(epoll, libc::EPOLL_CTL_ADD, wake, &raw mut event) } < 0 {
            let errno = crate::errno::get();
            unsafe {
                libc::close(wake);
                libc::close(epoll)
            };
            return Err(errno);
        }
        Ok((
            Self {
                epoll,
                wake,
                poll_only: Mutex::new(HashSet::new()),
            },
            epoll,
        ))
    }

    fn wake(&self) {
        wake_fd(self.wake)
    }
    fn drain_wake(&self) {
        drain_fd(self.wake)
    }

    fn update(&self, fd: c_int, before: i16, after: i16) -> Result<(), c_int> {
        if before == after {
            return Ok(());
        }
        let mut poll_only = self.poll_only.lock().unwrap_or_else(|e| e.into_inner());
        if poll_only.contains(&fd) {
            if after == 0 {
                poll_only.remove(&fd);
            }
            return Ok(());
        }
        let result = match (before != 0, after != 0) {
            (false, true) => Self::ctl(self.epoll, libc::EPOLL_CTL_ADD, fd, after),
            (true, true) => Self::ctl(self.epoll, libc::EPOLL_CTL_MOD, fd, after),
            (true, false) => Self::ctl(self.epoll, libc::EPOLL_CTL_DEL, fd, 0),
            (false, false) => Ok(()),
        };
        match result {
            Err(errno) if before == 0 && errno == libc::EPERM => {
                poll_only.insert(fd);
                Ok(())
            }
            // Closing a watched FD removes it from epoll. Keep Darwin's
            // deletion semantics even if the host has already done that work.
            Err(errno) if after == 0 && matches!(errno, libc::ENOENT | libc::EBADF) => Ok(()),
            other => other,
        }
    }

    fn wait(
        &self,
        queue: c_int,
        registrations: &HashMap<(usize, i16), Registration>,
        capacity: usize,
        timeout: c_int,
    ) -> Result<Vec<Ready>, c_int> {
        let mut result = Vec::new();
        let poll_only = self
            .poll_only
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if poll_only.is_empty() {
            return self.wait_epoll(queue, capacity, timeout);
        }
        let mut watched = Vec::with_capacity(poll_only.len() + 1);
        watched.push(libc::pollfd {
            fd: queue,
            events: libc::POLLIN,
            revents: 0,
        });
        for fd in &poll_only {
            let interest = interest_for(registrations, *fd);
            if interest != 0 {
                watched.push(libc::pollfd {
                    fd: *fd,
                    events: interest,
                    revents: 0,
                });
            }
        }
        if unsafe { libc::poll(watched.as_mut_ptr(), watched.len() as libc::nfds_t, timeout) } < 0 {
            return Err(crate::errno::get());
        }
        for entry in watched.iter().skip(1) {
            if entry.revents != 0 {
                result.push(Ready {
                    fd: entry.fd,
                    revents: entry.revents,
                    wake: false,
                });
            }
        }
        if watched[0].revents != 0 {
            result.extend(self.wait_epoll(
                queue,
                capacity.saturating_sub(result.len()).max(1),
                0,
            )?);
        }
        Ok(result)
    }
}

#[cfg(target_os = "linux")]
impl EpollBackend {
    fn wait_epoll(
        &self,
        queue: c_int,
        capacity: usize,
        timeout: c_int,
    ) -> Result<Vec<Ready>, c_int> {
        let mut events = vec![libc::epoll_event { events: 0, u64: 0 }; capacity.max(1)];
        let count =
            unsafe { libc::epoll_wait(queue, events.as_mut_ptr(), events.len() as c_int, timeout) };
        if count < 0 {
            return Err(crate::errno::get());
        }
        Ok(events
            .into_iter()
            .take(count as usize)
            .map(|event| Ready {
                fd: event.u64 as c_int,
                revents: epoll_revents(event.events),
                wake: event.u64 == Self::WAKE_TOKEN,
            })
            .collect())
    }
}

#[cfg(target_os = "linux")]
fn epoll_revents(events: u32) -> i16 {
    ((events & libc::EPOLLIN as u32 != 0) as i16 * libc::POLLIN)
        | ((events & libc::EPOLLOUT as u32 != 0) as i16 * libc::POLLOUT)
        | ((events & libc::EPOLLERR as u32 != 0) as i16 * libc::POLLERR)
        | ((events & libc::EPOLLHUP as u32 != 0) as i16 * libc::POLLHUP)
}

#[cfg(target_os = "linux")]
type SelectedBackend = EpollBackend;
#[cfg(not(target_os = "linux"))]
type SelectedBackend = PollBackend;

fn queues() -> &'static Mutex<HashMap<c_int, Arc<Mutex<Queue<SelectedBackend>>>>> {
    #[dynamic]
    static QUEUES: Mutex<HashMap<c_int, Arc<Mutex<Queue<SelectedBackend>>>>> = Default::default();
    &QUEUES
}

fn queue_of(fd: c_int) -> Option<Arc<Mutex<Queue<SelectedBackend>>>> {
    queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&fd)
        .cloned()
}

/// `int kqueue(void)`
pub extern "C" fn kqueue() -> c_int {
    let Ok((backend, fd)) = SelectedBackend::new() else {
        return -1;
    };
    // Whatever this number held before is gone with the descriptor it named;
    // see the module note on `close`.
    queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(fd, Arc::new(Mutex::new(Queue::new(backend))));
    fd
}

/// Wake a `kevent` blocked on `queue`.
fn wake_fd(queue: c_int) {
    let one: u64 = 1;
    // SAFETY: an eventfd this module created, and eight bytes to write.
    unsafe { libc::write(queue, (&raw const one).cast::<c_void>(), size_of::<u64>()) };
}

/// Drain the wakeup channel, which is level-triggered and would otherwise
/// return from every later `poll` at once.
fn drain_fd(queue: c_int) {
    let mut value: u64 = 0;
    // SAFETY: as [`wake`]; the descriptor is non-blocking, so this ends with
    // `EAGAIN` rather than waiting.
    while unsafe { libc::read(queue, (&raw mut value).cast::<c_void>(), size_of::<u64>()) } > 0 {}
}

/// Apply one change, answering with the `errno` it failed with.
///
/// The number is **Darwin's**: it reaches the guest as an `EV_ERROR` event's
/// `data`, which is a value rather than the `errno` slot, so nothing later
/// translates it.
fn apply<B: ReadinessBackend>(
    state: &mut Queue<B>,
    queue: c_int,
    change: &Kevent,
) -> Result<(), c_int> {
    // Two filters go out to their own `apply_*` before the `matches!` below, and
    // for the same reason each time: what becomes *readable* for them is not
    // their `ident`. `EVFILT_PROC`'s `ident` is a pid and the thing waited on is
    // a `pidfd`; `EVFILT_VNODE`'s `ident` is a descriptor and the thing waited on
    // is the queue's own `inotify` instance, because `inotify_add_watch` takes a
    // pathname rather than a descriptor. Both are `pub` in
    // [`super::mach::kevent64`], where the XNU citations are.
    if change.filter == EVFILT_PROC {
        return apply_proc(state, change);
    }
    if change.filter == EVFILT_VNODE {
        return apply_vnode(state, queue, change);
    }
    if !matches!(change.filter, EVFILT_READ | EVFILT_WRITE | EVFILT_USER) {
        return Err(refused_filter(change));
    }
    let key = (change.ident, change.filter);

    let mut registrations = state.registrations.clone();
    let mut triggered = state.triggered.clone();
    let mut triggered_set = state.triggered_set.clone();

    if change.flags & EV_DELETE != 0 {
        if registrations.remove(&key).is_none() {
            return Err(crate::errno::from_host(libc::ENOENT));
        }
        triggered.retain(|ident| *ident != change.ident);
        triggered_set.remove(&change.ident);
        sync_interest(state, &registrations, change.filter, change.ident)?;
        state.registrations = registrations;
        state.triggered = triggered;
        state.triggered_set = triggered_set;
        return Ok(());
    }

    if change.flags & EV_ADD != 0 {
        // A readiness filter names a descriptor, and one that is not open is
        // `EBADF` at registration rather than at the first `poll` - which is
        // where BSD reports it, and the only place a caller looks.
        if change.filter != EVFILT_USER {
            let fd =
                c_int::try_from(change.ident).map_err(|_| crate::errno::from_host(libc::EBADF))?;
            if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
                return Err(crate::errno::from_host(crate::errno::get()));
            }
        }
        registrations.insert(
            key,
            Registration {
                ident: change.ident,
                filter: change.filter,
                flags: change.flags,
                fflags: change.fflags,
                udata: change.udata,
                enabled: change.flags & EV_DISABLE == 0,
                proc: None,
                vnode: None,
            },
        );
    } else {
        let Some(registration) = registrations.get_mut(&key) else {
            return Err(crate::errno::from_host(libc::ENOENT));
        };
        if change.flags & EV_ENABLE != 0 {
            registration.enabled = true;
        }
        if change.flags & EV_DISABLE != 0 {
            registration.enabled = false;
        }
    }

    if change.filter == EVFILT_USER && change.fflags & NOTE_TRIGGER != 0 {
        let enabled = registrations
            .get(&key)
            .is_some_and(|registration| registration.enabled);
        if enabled && triggered_set.insert(change.ident) {
            triggered.push_back(change.ident);
        }
    }
    sync_interest(state, &registrations, change.filter, change.ident)?;
    let woke = change.filter == EVFILT_USER
        && change.fflags & NOTE_TRIGGER != 0
        && triggered != state.triggered;
    state.registrations = registrations;
    state.triggered = triggered;
    state.triggered_set = triggered_set;
    if woke {
        state.backend.wake();
    }
    Ok(())
}

/// Apply one `EVFILT_PROC` change.
///
/// **The decision is [`super::mach::kevent64::proc_change`]'s**, which is where
/// the XNU citations for it are; this only carries the decision out against this
/// queue's own `registrations` table, and opens the `pidfd` that the
/// registration then holds. Every step a general kqueue would take for a
/// descriptor is taken here for a `pidfd` - `EV_DELETE` closes it, `EV_DISABLE`
/// takes it out of the wait and `EV_ENABLE` puts it back - so that the filter
/// behaves like the others from the caller's side.
///
/// The transaction is the same one [`apply`] uses: the table is cloned, the
/// change is carried out against the copy, and the copy is installed only if
/// nothing failed. A change that is refused must leave the queue exactly as it
/// was, because the caller is entitled to carry on with the registrations it
/// already has.
fn apply_proc<B: ReadinessBackend>(state: &mut Queue<B>, change: &Kevent) -> Result<(), c_int> {
    use super::mach::kevent64::{ProcChange, pidfd_open, proc_change};

    let ident = change.ident;
    let key = (ident, EVFILT_PROC);
    let mut registrations = state.registrations.clone();
    // **Whether a change creates a registration or changes one is the queue's
    // question, not the decision's** - only the queue has the table - and
    // [`apply`] asks it the same way for every other filter: a change with no
    // `EV_ADD` for a registration that is not there is `ENOENT`, which is what
    // `kevent_register` answers with when `kq_find_knote_and_kq_lock` finds
    // nothing and `EV_ADD` was not given. Without this, a change that carried
    // only `EV_RECEIPT` would be read as a request to watch a process, and this
    // would answer "watching" where Darwin answers "no such registration".
    if change.flags & EV_ADD == 0 && !registrations.contains_key(&key) {
        super::mach::kevent64::announce_missing_proc(ident as u64);
        return Err(crate::errno::from_host(libc::ENOENT));
    }
    match proc_change(ident as u64, change.flags, change.fflags, change.udata as u64) {
        ProcChange::Refused(errno) => return Err(errno),
        ProcChange::Delete => {
            // Dropping the queue's `Arc<PidFd>` is the `close`, and it is the
            // only close: a wait in flight on another thread holds its own, so
            // the descriptor cannot be closed underneath a `poll`.
            if registrations.remove(&key).is_none() {
                super::mach::kevent64::announce_missing_proc(ident as u64);
                return Err(crate::errno::from_host(libc::ENOENT));
            }
        }
        ProcChange::Armed { enabled } => {
            let Some(existing) = registrations.get(&key) else {
                super::mach::kevent64::announce_missing_proc(ident as u64);
                return Err(crate::errno::from_host(libc::ENOENT));
            };
            // Enabling and disabling take the registration out of the wait and
            // put it back, and the `pidfd` is untouched by either: the process
            // did not stop existing while a knote was disabled, and the
            // descriptor is what the knote *is*.
            let proc = super::mach::kevent64::Proc::rearmed(
                existing
                    .proc
                    .as_ref()
                    .expect("an EVFILT_PROC registration holds its pidfd"),
                enabled,
            );
            registrations.insert(
                key,
                Registration {
                    enabled,
                    proc: Some(Arc::new(proc)),
                    ..existing.clone()
                },
            );
        }
        ProcChange::Watch { pid, enabled, udata } => {
            // A re-`EV_ADD` on the same `(ident, EVFILT_PROC)` keeps the
            // descriptor that is already there: "there may be only be one unique
            // kevent per kqueue" means the registration is replaced, not stacked.
            let proc = match registrations.get(&key).and_then(|r| r.proc.as_ref()) {
                Some(existing) => super::mach::kevent64::Proc::sharing_of(
                    existing, pid, enabled, udata,
                ),
                None => {
                    let fd = pidfd_open(pid).map_err(|errno| {
                        if errno == crate::errno::from_host(libc::ENOSYS) {
                            super::mach::kevent64::refused_proc_unavailable(ident as u64);
                        }
                        errno
                    })?;
                    let proc = super::mach::kevent64::Proc::new(fd, pid, enabled, udata);
                    announce_proc_registered(change, &proc);
                    proc
                }
            };
            registrations.insert(
                key,
                Registration {
                    ident,
                    filter: EVFILT_PROC,
                    flags: change.flags,
                    fflags: change.fflags,
                    udata: change.udata,
                    enabled,
                    proc: Some(Arc::new(proc)),
                    vnode: None,
                },
            );
        }
    }
    sync_interest(state, &registrations, EVFILT_PROC, ident)?;
    state.registrations = registrations;
    Ok(())
}

/// Apply one `EVFILT_VNODE` change.
///
/// **The decision is [`super::mach::kevent64::vnode_change`]'s**, which is where
/// the XNU citations for the filter, the `NOTE_` values and the reasons three of
/// them cannot be reported on this host live; this carries the decision out
/// against this queue's own table and opens the `inotify` watch the registration
/// then holds. The transaction is the one [`apply`] and [`apply_proc`] use, and
/// for the same stated reason: a refused change leaves the queue exactly as it
/// was, because `registrations` is a clone that is only committed at the end.
///
/// **One `inotify` instance per kqueue, created here and released here.** That is
/// what makes a change to this queue's watched file wake *this* queue and no
/// other. A process-wide instance would put this queue's descriptor into a
/// readiness set because of somebody else's watch, which is a readiness report
/// about a queue that had no event - the shape of thing this module refuses.
/// `state.inotify` is the only place that knows which instance this queue uses;
/// each registration reaches the same `Arc` through its own [`Vnode`], so
/// [`kevent`] drains the one instance whose events this queue is waiting for and
/// an `inotify` cannot outlive its last registration.
fn apply_vnode<B: ReadinessBackend>(
    state: &mut Queue<B>,
    queue: c_int,
    change: &Kevent,
) -> Result<(), c_int> {
    use super::mach::kevent64::{
        Inotify, Vnode, VnodeChange, announce_missing_vnode, announce_vnode_note_dropped,
        vnode_change,
    };

    let ident = change.ident;
    let key = (ident, EVFILT_VNODE);
    let mut registrations = state.registrations.clone();
    // The queue's own question, asked the same way `apply` and `apply_proc` ask
    // it: a change with no `EV_ADD` for a registration that is not there is
    // `ENOENT`, which is what `kevent_register` answers when
    // `kq_find_knote_and_kq_lock` finds nothing. Without this a change carrying
    // only `EV_RECEIPT` would be read as a request to watch a path, and the
    // caller would get a *success* receipt for a watch that was never made -
    // which is how `EV_RECEIPT` becomes a lie.
    if change.flags & EV_ADD == 0 && !registrations.contains_key(&key) {
        announce_missing_vnode(ident as u64);
        return Err(crate::errno::from_host(libc::ENOENT));
    }
    match vnode_change(ident as u64, change.flags, change.fflags, change.udata as u64) {
        VnodeChange::Refused(errno) => return Err(errno),
        VnodeChange::Delete => {
            // Dropping the queue's `Arc<Vnode>` *is* the `inotify_rm_watch`, and
            // it is the only one: a `drain` in flight on another thread holds its
            // own `Arc`, so the watch cannot be removed from under it.
            if registrations.remove(&key).is_none() {
                announce_missing_vnode(ident as u64);
                return Err(crate::errno::from_host(libc::ENOENT));
            }
        }
        VnodeChange::Armed { enabled } => {
            // Enabling and disabling take the registration out of the wait and
            // put it back, and the watch is untouched by either: the object did
            // not stop being watched while the knote was disabled, and the watch
            // is what the knote *is*. Notes that arrived while it was disabled
            // are kept, because XNU keeps `kn_fflags` across a disable too -
            // `filt_vnodetouch` only reassigns `kn_sfflags`.
            let Some(existing) = registrations.get(&key) else {
                announce_missing_vnode(ident as u64);
                return Err(crate::errno::from_host(libc::ENOENT));
            };
            let vnode = existing
                .vnode
                .clone()
                .expect("an EVFILT_VNODE registration holds its inotify watch");
            registrations.insert(
                key,
                Registration {
                    enabled,
                    vnode: Some(vnode),
                    ..existing.clone()
                },
            );
        }
        VnodeChange::Watch {
            mask,
            dropped,
            enabled,
            udata,
        } => {
            // **Every note dropped from the mask is named before the registration
            // is made**, so the log says what the watcher is *not* being told
            // about even if the run never gets as far as a delivery.
            if dropped != 0 {
                announce_vnode_note_dropped(ident as u64, change.fflags, dropped);
            }
            // A re-`EV_ADD` on the same key replaces the registration, which means
            // the old watch goes: "there may be only be one unique kevent per
            // kqueue". Removing it *before* opening the new one is what stops a
            // re-`EV_ADD` of a narrowed mask from leaving the old, wider interest
            // behind it.
            registrations.remove(&key);
            let inotify = match state.inotify.clone() {
                Some(inotify) => inotify,
                None => {
                    let created = Arc::new(Inotify::new()?);
                    state.inotify = Some(Arc::clone(&created));
                    created
                }
            };
            let vnode = Vnode::watch(queue, inotify, ident as u64, mask, udata)?;
            super::mach::kevent64::announce_vnode_registered(&vnode);
            registrations.insert(
                key,
                Registration {
                    ident,
                    filter: EVFILT_VNODE,
                    flags: change.flags,
                    // The *narrowed* interest, not the caller's word: this is the
                    // `kn_sfflags` the knote is given, and it is what
                    // `filt_vnode_common` intersects an arriving hint with. So a
                    // re-read of the registration reports what is actually
                    // watched rather than what was asked for.
                    fflags: mask,
                    udata: change.udata,
                    enabled,
                    proc: None,
                    vnode: Some(vnode),
                },
            );
        }
    }
    // The instance is released with the last vnode registration on this queue, so
    // a kqueue that watched something and then stopped does not hold a descriptor
    // for the rest of the process.
    if !registrations
        .values()
        .any(|registration| registration.filter == EVFILT_VNODE)
    {
        state.inotify = None;
    }
    sync_interest(state, &registrations, EVFILT_VNODE, ident)?;
    state.registrations = registrations;
    Ok(())
}

fn interest_for(registrations: &HashMap<(usize, i16), Registration>, fd: c_int) -> i16 {
    registrations
        .values()
        .filter(|registration| registration.enabled && registration.watches(fd))
        .fold(0, |interest, registration| {
            interest
                | match registration.filter {
                    // A vnode change is a thing that *happened*, so the `inotify`
                    // instance is read, not written - and `EPOLLOUT` on it would
                    // be a wait for a queue that can never be written to.
                    EVFILT_READ | EVFILT_PROC | EVFILT_VNODE => libc::POLLIN,
                    EVFILT_WRITE => libc::POLLOUT,
                    _ => 0,
                }
        })
}

fn sync_interest<B: ReadinessBackend>(
    state: &mut Queue<B>,
    registrations: &HashMap<(usize, i16), Registration>,
    filter: i16,
    ident: usize,
) -> Result<(), c_int> {
    if filter == EVFILT_USER {
        return Ok(());
    }
    // The descriptor this key is watched through, in whichever version of the
    // table still has it: an `EV_DELETE` has already taken it out of
    // `registrations`, and the point of the update is to un-arm the descriptor it
    // was armed on. For a readiness filter that is the `ident`; for
    // `EVFILT_PROC` it is the `pidfd`, and the `ident` is a pid that means
    // nothing to `poll` or to `epoll_ctl`.
    let key = (ident, filter);
    let fd = registrations
        .get(&key)
        .and_then(descriptor_of)
        .or_else(|| state.registrations.get(&key).and_then(descriptor_of))
        .or_else(|| c_int::try_from(ident).ok())
        .ok_or_else(|| crate::errno::from_host(libc::EBADF))?;
    state
        .backend
        .update(
            fd,
            interest_for(&state.registrations, fd),
            interest_for(registrations, fd),
        )
        .map_err(crate::errno::from_host)
}

/// Say that an `EVFILT_PROC` registration was made, and what it is watching.
///
/// **Off by default, and env-gated on `ROSETTA3_TRACE_EVFILT_PROC`.** It exists
/// because the alternative to knowing whether this filter is reached in a run is
/// guessing - and guessing is what the last two rounds of this milestone have
/// been doing. A `--headless` run that registers nothing here says nothing at all
/// about the filter; a run that registers and then dies somewhere else says the
/// filter is not the wall. Both are only knowable by printing, and the whole
/// lesson of this filter's history is that the interesting thing about it was
/// invisible: it was `EINVAL`ed with no message at all.
///
/// The variable is read through [`std::env::var_os`] and the function returns
/// immediately when it is unset, so the default path is a no-op and the default
/// output is unchanged.
fn announce_proc_registered(change: &Kevent, proc: &super::mach::kevent64::Proc) {
    if std::env::var_os("ROSETTA3_TRACE_EVFILT_PROC").is_none() {
        return;
    }
    eprintln!(
        "rosetta3: kqueue: EVFILT_PROC registration for pid {:#x} ({}) on a pidfd {}, udata {:#x}, \
         {} - the filter is answered here, and this line is the only proof that it was reached",
        proc.pid(),
        if proc.is_enabled() {
            "armed"
        } else {
            "registered but EV_DISABLED"
        },
        proc.raw_fd(),
        change.udata,
        if proc.raw_fd() as usize == change.ident {
            "and the pid and the descriptor are the same number, which is the collision this \
             module refuses to confuse"
        } else {
            "and the pid is not the descriptor number, as it must not be"
        }
    );
}

/// The filter's name, for a refusal.
///
/// Anything not named here stays its number: inventing a name for a filter this
/// file has never heard of is how a diagnostic starts lying.
fn filter_name(filter: i16) -> &'static str {
    match filter {
        EVFILT_READ => "EVFILT_READ",
        EVFILT_WRITE => "EVFILT_WRITE",
        -3 => "EVFILT_AIO",
        -4 => "EVFILT_VNODE",
        -6 => "EVFILT_SIGNAL",
        -7 => "EVFILT_TIMER",
        -8 => "EVFILT_MACHPORT",
        -9 => "EVFILT_FS",
        -11 => "EVFILT_DEVICE",
        -12 => "EVFILT_VM",
        -13 => "EVFILT_WORKLOOP",
        -14 => "EVFILT_USER_EVENT",
        -15 => "EVFILT_EXCEPT",
        _ => "a filter kqueue does not define",
    }
}

/// A filter this module will not answer, **named, per filter, and counted**.
///
/// This used to be a bare `EINVAL` out of a `matches!`, and that is the worst
/// shape a refusal can take in this tree: the number reached the guest as an
/// `EV_ERROR` event's `data` with nothing anywhere saying *which capability* was
/// missing, so the log could not distinguish "the guest asked for something we
/// do not have" from "the guest is wrong".
///
/// Two defects in the version that replaced it are fixed here.
///
/// **It had one `Once` for the whole refused set**, so a run that refused
/// `EVFILT_VNODE` four times and then `EVFILT_AIO` once named only the first of
/// them - and it carried no count, so "this happened" could never become "this
/// happened four times". The count is per filter, and the first eight of each
/// print one at a time (a small exact number is worth having) and after that
/// every thousandth, so a run that refuses in a loop cannot turn the log into the
/// wall.
///
/// **It also asserted the opposite of the truth about the `ident`.** It read
/// `EVFILT_VNODE` refusals as refusing a *Mach port*, when the `ident` those
/// registrations carry is `0x23` - an **open descriptor**, the one
/// `FilePathWatcherKQueue` opened with `O_EVTONLY`. A Mach port and an open
/// descriptor are different facts about the same number, and the difference is
/// the difference between a refusal that costs nothing and a missing capability,
/// so the message now says which one this is, by looking rather than assuming -
/// see [`super::mach::kevent64::describe_ident`], which is the one derivation of
/// that classification in the crate.
///
/// And the census that used to close this function is **corrected**: it is *not*
/// true that `EVFILT_PROC` was the only refused filter anything waited on.
/// `EVFILT_VNODE` is registered *and* waited on; see this file's module note for
/// the four call sites and the counts.
fn refused_filter(change: &Kevent) -> c_int {
    use std::sync::OnceLock;
    static COUNTS: OnceLock<Mutex<HashMap<i16, u32>>> = OnceLock::new();
    let nth = {
        let mut counts = COUNTS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let slot = counts.entry(change.filter).or_insert(0);
        *slot += 1;
        *slot
    };
    if nth <= 8 || nth % 1000 == 0 {
        eprintln!(
            "rosetta3: kevent refused {} ({}) with {} on ident {:#x}, which is {} \
             [refusal {nth} of this filter in this process]. This kevent answers EVFILT_READ (-1) \
             and EVFILT_WRITE (-2) as poll readiness on a real descriptor, EVFILT_USER (-10) as a \
             self-triggered event, EVFILT_PROC (-5) as \"has this process exited\" on a real \
             pidfd, and EVFILT_VNODE (-4) as a note set on a real inotify watch; every other \
             filter is refused here by name. EVFILT_FS (-9) names a mount, EVFILT_AIO (-3) an I/O \
             completion, EVFILT_SIGNAL (-6) a signal delivery, EVFILT_VM (-12) a vmspace and \
             EVFILT_EXCEPT (-15) an exception port - none of which this runtime has anything to \
             attach to; EVFILT_TIMER (-7) and EVFILT_MACHPORT (-8) are answered by kevent64 for \
             their own callers; EVFILT_DEVICE (-11), EVFILT_WORKLOOP (-13) and EVFILT_USER_EVENT \
             (-14) name subsystems it does not model. EVFILT_VNODE was in this list and is not: \
             the notes it cannot produce are refused there by name rather than mapped to the \
             nearest event. The full census of what the guest passes, and which of it is actually \
             waited on, is in this file's module note",
            filter_name(change.filter),
            change.filter,
            flags_meaning(change.flags),
            change.ident,
            super::mach::kevent64::describe_ident(change.ident as u64),
        );
    }
    crate::errno::from_host(libc::EINVAL)
}

/// The filter's `flags` word, spelled out.
///
/// **Only the action bits, because only they say what the change asks for:**
/// `EV_ADD` starts a registration and `EV_DELETE` ends one, and that difference
/// is most of whether a refusal can cost a hang. A hex number the reader has to
/// decode is much the same silence as no name at all, and the two words this
/// module actually sees from the framework - `0xfffc` and `0xfffb` - differ only
/// in the bits named here.
fn flags_meaning(flags: u16) -> String {
    const NAMES: [(u16, &str); 10] = [
        (EV_ADD, "EV_ADD"),
        (EV_DELETE, "EV_DELETE"),
        (EV_ENABLE, "EV_ENABLE"),
        (EV_DISABLE, "EV_DISABLE"),
        (EV_ONESHOT, "EV_ONESHOT"),
        (EV_CLEAR, "EV_CLEAR"),
        (EV_RECEIPT, "EV_RECEIPT"),
        (EV_DISPATCH, "EV_DISPATCH"),
        (EV_ERROR, "EV_ERROR"),
        (EV_EOF, "EV_EOF"),
    ];
    let mut out = String::new();
    for (bit, name) in NAMES {
        if flags & bit != 0 {
            if !out.is_empty() {
                out.push('|');
            }
            out.push_str(name);
        }
    }
    if out.is_empty() {
        out.push_str("no action bit");
    }
    out.push_str(&format!(" ({flags:#06x})"));
    out
}

/// The `poll` timeout in milliseconds, or -1 for "wait forever".
///
/// Rounded **up**: a caller asking for half a millisecond asked to wait, and a
/// zero here would make its wait a poll.
fn timeout_ms(timeout: *const libc::timespec) -> Result<c_int, c_int> {
    // SAFETY: null or the caller's `struct timespec`, whose layout is the same
    // two 64-bit words on both platforms.
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

/// The descriptor one registration waits on.
///
/// **One registration, one descriptor - and for `EVFILT_PROC` it is not the
/// `ident`.** Every other filter's `ident` *is* its descriptor number; an
/// `EVFILT_PROC`'s `ident` is a pid, and the thing `poll` can wait on is the
/// `pidfd` the registration is holding. Keeping that in one function is what
/// stops a pid from being waited on as though it were a descriptor, or a
/// `pidfd` from being reported for a registration that never named it.
fn descriptor_of(registration: &Registration) -> Option<c_int> {
    match registration.filter {
        EVFILT_PROC => registration.proc.as_ref().map(|proc| proc.raw_fd()),
        // The queue's `inotify` instance. Every vnode registration on a queue
        // folds onto this one descriptor, so the first one registered puts it in
        // the readiness set and the last one deleted takes it out again - which
        // is what `sync_interest` and `interest_for` already do for a shared
        // descriptor, and why no new machinery was needed for it.
        EVFILT_VNODE => registration.vnode.as_ref().map(|watch| watch.inotify_fd()),
        _ => c_int::try_from(registration.ident).ok(),
    }
}

/// The descriptors to wait on: the wakeup channel, and every enabled readiness
/// registration folded together by descriptor.
#[cfg(not(target_os = "linux"))]
fn poll_set(
    queue: c_int,
    registrations: &HashMap<(usize, i16), Registration>,
) -> Vec<libc::pollfd> {
    let mut interest: HashMap<c_int, i16> = HashMap::new();
    interest.insert(queue, libc::POLLIN);
    for registration in registrations.values() {
        if !registration.enabled || registration.filter == EVFILT_USER {
            continue;
        }
        // `descriptor_of` is what keeps an `EVFILT_PROC`'s pid out of the
        // descriptor number space here: the descriptor it waits on is the
        // `pidfd`, never the pid.
        let Some(fd) = descriptor_of(registration) else {
            continue;
        };
        *interest.entry(fd).or_default() |= match registration.filter {
            EVFILT_READ | EVFILT_PROC => libc::POLLIN,
            _ => libc::POLLOUT,
        };
    }
    interest
        .into_iter()
        .map(|(fd, events)| libc::pollfd {
            fd,
            events,
            revents: 0,
        })
        .collect()
}

/// How many bytes can be read from `fd`, which is what `EVFILT_READ` reports as
/// its `data` and what a caller sizes its next read from.
fn readable_bytes(fd: c_int) -> isize {
    let mut bytes: c_int = 0;
    // SAFETY: `FIONREAD` writes one `int` through the pointer it is given.
    match unsafe { libc::ioctl(fd, libc::FIONREAD, &raw mut bytes) } {
        0 => bytes as isize,
        _ => 0,
    }
}

/// `int kevent(int kq, const struct kevent *changelist, int nchanges,
/// struct kevent *eventlist, int nevents, const struct timespec *timeout)`
///
/// # Safety
///
/// `changes` holds `nchanges` events and `events` has room for `nevents`.
pub unsafe extern "C" fn kevent(
    queue: c_int,
    changes: *const Kevent,
    nchanges: c_int,
    events: *mut Kevent,
    nevents: c_int,
    timeout: *const libc::timespec,
) -> c_int {
    if nchanges < 0
        || nevents < 0
        || (nchanges != 0 && changes.is_null())
        || (nevents != 0 && events.is_null())
    {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    let Some(state) = queue_of(queue) else {
        crate::errno::set(libc::EBADF);
        return -1;
    };
    let capacity = nevents as usize;
    let mut out: Vec<Kevent> = Vec::with_capacity(capacity);

    let changes = match nchanges {
        0 => &[][..],
        // SAFETY: the caller states there are `nchanges` of them.
        n => unsafe { std::slice::from_raw_parts(changes, n as usize) },
    };
    // The runtime half of the census in this file's module note: one line per
    // distinct call *shape* a run reaches, with whether the call had room for an
    // event - which is what decides whether anything refused in it could have
    // hung a thread. Off unless `ROSETTA3_TRACE_KEVENT` is set.
    announce_call_shape(changes, nchanges, nevents, timeout);
    {
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        for change in changes {
            let failure = apply(&mut state, queue, change).err();
            // `EV_RECEIPT` asks for the *outcome of every change* rather than
            // for readiness, and it asks for it whether or not the change
            // failed - which is how a caller applies a batch and finds out
            // which one of them the kernel rejected.
            if change.flags & EV_RECEIPT != 0 {
                if out.len() < capacity {
                    out.push(Kevent {
                        flags: EV_ERROR,
                        data: failure.unwrap_or(0) as isize,
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
        unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), events, out.len()) };
        return out.len() as c_int;
    }

    let triggered = !state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .triggered
        .is_empty();
    // Something is already waiting, so this is a collection rather than a wait.
    let wait = match triggered {
        true => 0,
        false => match timeout_ms(timeout) {
            Ok(ms) => ms,
            Err(errno) => {
                crate::errno::set(crate::errno::to_host(errno));
                return -1;
            }
        },
    };
    let ready = {
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        let backend = Arc::clone(&state.backend);
        let registrations = state.registrations.clone();
        drop(state);
        match backend.wait(queue, &registrations, capacity, wait) {
            Ok(ready) => ready,
            Err(errno) => {
                crate::errno::set(errno);
                return -1;
            }
        }
    };

    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    if ready.iter().any(|entry| entry.wake) {
        state.backend.drain_wake();
    }

    // The `inotify` events are folded into each watch's pending note word
    // **before** any registration is asked whether it has something to report,
    // because `poll` reported that this queue's `inotify` is readable and the
    // only question left is which notes that stands for.
    //
    // **This is not behind the trace variable, and it must not be.** A drain
    // gated on a diagnostic switch is a filter that never fires while the
    // variable is unset, which is the one outcome that looks like success and is
    // not: the registration would be accepted, the queue would be armed, the
    // descriptor would become readable, and no event would ever arrive. The
    // switch goes on the *reporting*, below, and nowhere else.
    //
    // The queue lock is held across it, and that is safe because the order is the
    // same everywhere: `queue -> inotify -> watch` in `apply_vnode` and
    // `inotify -> watch` in [`Inotify::drain`], and nothing takes a watch lock
    // before an `inotify` one, so there is no cycle. The `Arc`s are taken under
    // the `inotify` lock and used after it has been dropped.
    let vnode_fired = match state.inotify.clone() {
        Some(inotify) => inotify.drain(),
        None => 0,
    };
    if vnode_fired != 0 && vnode_trace_wanted() {
        eprintln!(
            "rosetta3: kqueue: {vnode_fired} EVFILT_VNODE watch(es) gained a note on kqueue \
             {queue} - this is the proof that the filter fires, and it is the only one"
        );
    }

    // The user events first: a program triggers one to be told to stop, and a
    // busy descriptor must not keep that behind it.
    while out.len() < capacity {
        let Some(ident) = state.triggered.pop_front() else {
            break;
        };
        state.triggered_set.remove(&ident);
        if let Some(registration) = state.registrations.get(&(ident, EVFILT_USER))
            && registration.enabled
        {
            out.push(registration.fired());
        }
    }

    for entry in ready {
        if out.len() == capacity || entry.wake || entry.revents == 0 {
            continue;
        }
        let firing: Vec<((usize, i16), Registration)> = state
            .registrations
            .iter()
            // `watches`, not `r.ident == fd`: an `EVFILT_PROC`'s descriptor is
            // its `pidfd`, and matching on the pid instead would report another
            // registration's readiness whenever a pid and a descriptor number
            // happened to be equal.
            .filter(|(_, r)| r.enabled && r.watches(entry.fd))
            .map(|(key, r)| (*key, r.clone()))
            .collect();
        for (key, registration) in firing {
            if out.len() == capacity {
                break;
            }
            // **An `EVFILT_VNODE` is not a `poll` revent and is not derived from
            // one.** What `entry.revents` reports is that this queue's `inotify`
            // instance is readable; what the event says is the note word the
            // matching watch has accumulated, which is a fact about a *different*
            // object than the descriptor that became readable. So it is asked for
            // rather than computed, and it is handled before `wanted` because
            // there is no `wanted` for it: a note is not a readiness condition.
            //
            // Note that `entry.revents == 0` is filtered out above, and a
            // vnode `inotify` is armed `POLLIN`, so an `EPOLLIN` here is the
            // only reason to get this far.
            if registration.filter == EVFILT_VNODE {
                let Some(event) = registration.vnode_fired() else {
                    // The instance was readable for another watch on this queue,
                    // or for an event whose note is not in this registration's
                    // interest. Either way there is no event to report, and
                    // reporting one with an empty `fflags` would be a readiness
                    // the caller cannot act on.
                    continue;
                };
                out.push(event);
                // **`NOTE_REVOKE` is one-shot whether or not the caller asked.**
                // `filt_vnode_common` sets `EV_EOF | EV_ONESHOT` on it, because
                // the vnode XNU would have attached the knote to is gone:
                // `if (NOTE_REVOKE == hint) kn->kn_flags |= (EV_EOF | EV_ONESHOT);`
                // The delivery is therefore the last one this knote gives, and
                // leaving it armed would re-report a revoke that has already
                // happened - which is a busy watcher, and a wrong one.
                if registration.flags & EV_ONESHOT == 0
                    && event.fflags & super::mach::kevent64::VNODE_REVOKE == 0
                {
                    continue;
                }
                let before = interest_for(&state.registrations, entry.fd);
                state.registrations.remove(&key);
                let after = interest_for(&state.registrations, entry.fd);
                let _ = state.backend.update(entry.fd, before, after);
                continue;
            }
            let wanted = match registration.filter {
                EVFILT_READ | EVFILT_PROC => libc::POLLIN,
                EVFILT_WRITE => libc::POLLOUT,
                _ => continue,
            };
            // The three conditions `poll` reports whether or not they were
            // asked for. A hangup or an error is readiness as far as kqueue is
            // concerned - the read that follows is what reports it.
            if entry.revents & (wanted | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0 {
                continue;
            }
            let mut event = registration.fired();
            if entry.revents & libc::POLLNVAL != 0 {
                event.flags |= EV_ERROR;
                event.data = crate::errno::from_host(libc::EBADF) as isize;
            } else {
                // `EV_EOF` and the byte count are the *read* filter's answers and
                // are not invented for the others. An `EVFILT_PROC` event's
                // `EV_EOF` and its `EV_ONESHOT` are already in `event.flags`,
                // because XNU sets them itself - see `Proc::exited` - and
                // putting a second one there would be a word this module made up.
                if entry.revents & libc::POLLHUP != 0 && registration.filter != EVFILT_PROC {
                    event.flags |= EV_EOF;
                }
                if registration.filter == EVFILT_READ {
                    event.data = readable_bytes(entry.fd);
                }
            }
            out.push(event);
            // A delivered `EVFILT_PROC` is **always** one-shot, whether or not
            // the caller asked for it: the process is gone, and the knote XNU
            // would have attached to it is gone with it. And a `pidfd` stays
            // readable for as long as the descriptor lives, so leaving the
            // registration armed would re-report the same exit on every call
            // until the caller deleted it - which is the one way this delivery
            // could look like it worked and be wrong.
            if registration.filter == EVFILT_PROC {
                let before = interest_for(&state.registrations, entry.fd);
                state.registrations.remove(&key);
                let after = interest_for(&state.registrations, entry.fd);
                let _ = state.backend.update(entry.fd, before, after);
                continue;
            }
            // `EV_ONESHOT` deletes the registration once it has fired and
            // `EV_DISPATCH` disables it until it is enabled again. Both are how
            // a caller avoids being handed the same readiness on every turn.
            if registration.flags & EV_ONESHOT != 0 {
                let before = interest_for(&state.registrations, entry.fd);
                state.registrations.remove(&key);
                let after = interest_for(&state.registrations, entry.fd);
                let _ = state.backend.update(entry.fd, before, after);
            } else if registration.flags & EV_DISPATCH != 0 {
                let before = interest_for(&state.registrations, entry.fd);
                if let Some(registration) = state.registrations.get_mut(&key) {
                    registration.enabled = false;
                }
                let after = interest_for(&state.registrations, entry.fd);
                let _ = state.backend.update(entry.fd, before, after);
            }
        }
    }
    // SAFETY: the caller states there is room for `nevents`, and `out` never
    // grew past that.
    unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), events, out.len()) };
    out.len() as c_int
}

/// `kevent64` and the filters `poll` cannot answer; see the module note.
pub const REFUSED: &[&str] = &["kevent64", "kevent_qos"];

/// Whether the run asked for the `kevent` census.
///
/// Read **once** through a `OnceLock`, because the answer cannot change and this
/// is on the path of every call. With the variable unset - the default - the
/// census costs one atomic load and a `return`, and the default output is
/// unchanged byte for byte.
fn census_wanted() -> bool {
    use std::sync::OnceLock;
    static WANTED: OnceLock<bool> = OnceLock::new();
    *WANTED.get_or_init(|| std::env::var_os("ROSETTA3_TRACE_KEVENT").is_some())
}

/// Whether the run asked for the `EVFILT_VNODE` delivery trace.
///
/// **Off by default; env-gated on `ROSETTA3_TRACE_EVFILT_VNODE=1`.** Read once
/// through a [`OnceLock`] for the same reason as [`census_wanted`]: the answer
/// cannot change, and with the variable unset - the default - the line below
/// costs one atomic load and the default output is unchanged.
///
/// **It gates a line, never the drain.** The gate is on the *reporting*; the
/// `Inotify::drain` above runs whether or not this is set, because a drain
/// behind a diagnostic switch is a filter that is accepted, armed, and never
/// fires - which is the one outcome that looks like success and is not.
fn vnode_trace_wanted() -> bool {
    use std::sync::OnceLock;
    static WANTED: OnceLock<bool> = OnceLock::new();
    *WANTED.get_or_init(|| std::env::var_os("ROSETTA3_TRACE_EVFILT_VNODE").is_some())
}

/// The census, keyed by call shape. See [`announce_call_shape`].
static CENSUS: std::sync::OnceLock<Mutex<HashMap<(i16, u16, u32, bool, bool, bool), u32>>> =
    std::sync::OnceLock::new();

/// One line per distinct **shape** of `kevent` call, with a running count.
///
/// **Off by default; env-gated on `ROSETTA3_TRACE_KEVENT=1`.** It is the runtime
/// half of the census in this file's module note, and it exists because the
/// static half cannot answer the one question that decides whether a refusal is
/// a wall: *did this run reach it at all?* `EVFILT_VNODE` is registered **and**
/// waited on by two `FilePathWatcherKQueue` classes, and a run that constructs
/// neither looks identical to one that constructs both - in a disassembly and in
/// a refusal message alike. The measurement that it is reached *sometimes* and
/// not others, with the same binary and no code change, is in the module note.
///
/// "Shape" is everything that changes what the call asks for: the first change's
/// filter, flags and `fflags`, whether a change list was submitted at all, and
/// **whether there was room for an event** - which is what makes the call a
/// *wait* rather than a registration, and therefore what decides whether
/// anything refused in it could have hung a thread. **A `nevents == 0` call
/// cannot block**, and that single fact is the difference between a diagnostic and
/// a hang, so it is stated on every line.
///
/// The count is printed on the first sighting of a shape and at every power of
/// two after that, so the number converges without a line per call.
fn announce_call_shape(
    changes: &[Kevent],
    nchanges: c_int,
    nevents: c_int,
    timeout: *const libc::timespec,
) {
    if !census_wanted() {
        return;
    }
    type Key = (i16, u16, u32, bool, bool, bool);
    let first = changes.first();
    let key: Key = (
        first.map_or(0, |change| change.filter),
        first.map_or(0, |change| change.flags),
        first.map_or(0, |change| change.fflags),
        nchanges > 0,
        nevents > 0,
        !timeout.is_null(),
    );
    let nth = {
        let mut seen = CENSUS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let slot = seen.entry(key).or_insert(0);
        *slot += 1;
        *slot
    };
    // First sighting, then every doubling: 1, 2, 4, 8, ... 16, 32, 64, ...
    if !nth.is_power_of_two() {
        return;
    }
    let filter = match first {
        Some(change) => format!(
            "{} ({}) with {}",
            filter_name(change.filter),
            change.filter,
            flags_meaning(change.flags)
        ),
        None => "no change list at all".to_string(),
    };
    let verb = if nchanges > 0 && first.is_some_and(|change| change.flags & EV_ADD == 0) {
        "The change list carries no EV_ADD, so this is a deregistration of something this call \
         did not register."
    } else if nchanges > 0 {
        "The change list carries EV_ADD, so this call does register something."
    } else {
        "No change list, so this call can only collect or wait."
    };
    eprintln!(
        "rosetta3: kevent: {nth} call(s) of this shape - {filter}, fflags {:#x}, {nchanges} \
         change(s), {nevents} event slot(s) so {}, timeout {}. {verb}",
        first.map_or(0, |change| change.fflags),
        if nevents > 0 {
            "this call can block, so anything refused in it is a possible hang"
        } else {
            "this call cannot block, so nothing refused in it can hang a thread"
        },
        if timeout.is_null() {
            "NULL, which means wait forever"
        } else {
            "given, so it is a bounded wait"
        },
    );
}

exports::exports! {
    functions {
        "kqueue" => kqueue,
        "kevent" => kevent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zero timeout, which makes `kevent` a collection rather than a wait.
    static NO_WAIT: libc::timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };

    /// A fifth of a second, for a test that is waiting for something which has
    /// already happened and is checking that it is *not* reported again.
    static A_FIFTH: libc::timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 200_000_000,
    };

    /// A second, for a test that is waiting for a child to exit.
    static SECOND: libc::timespec = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };

    /// `NOTE_EXIT`, from `<sys/event.h>`: `#define NOTE_EXIT 0x80000000`.
    ///
    /// Repeated here rather than taken from `super::mach::kevent64`, so that a
    /// change to *that* module's constant cannot quietly change what this file's
    /// tests assert. The framework's own registration is the authority for the
    /// value: the top four bytes of `movabs rax,0x800000000001fffb`.
    const NOTE_EXIT: u32 = 0x8000_0000;

    /// `NOTE_EXITSTATUS`, which the header calls "exit status to be returned,
    /// valid for child process or when allowed to signal target pid". A
    /// `pidfd` cannot produce it, so it is refused rather than answered with a
    /// zero that would read as "exited with status 0".
    const NOTE_EXITSTATUS: u32 = 0x0400_0000;

    /// `EV_CLEAR`, from `<sys/event.h>`: `#define EV_CLEAR 0x0020`.
    ///
    /// Not a constant this module has - the other action bits are, because they
    /// are ones a caller passes in - and repeated here so the test can spell the
    /// delivered word out in full rather than referring to another module's
    /// constant for a bit whose origin is the point.
    const EV_CLEAR: u16 = 0x0020;

    /// A child that has already exited and been reaped, so its pid is free.
    ///
    /// A `pidfd` on a *zombie* is readable, and a `pidfd` on a pid that has
    /// been reaped and is gone cannot be opened at all - which is `ESRCH`, and
    /// which is the answer both of the framework's callers treat as "it is dead".
    /// So a test needs a pid in each of those two states: `reap` leaves a
    /// zombie this process has not reaped, `!reap` leaves nothing at all.
    fn child_that_exits(reap: bool) -> i32 {
        // SAFETY: `fork` takes no arguments and both this and the child return.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed: {}", crate::errno::get());
        if pid == 0 {
            // SAFETY: `_exit` in the child, so no atexit handler and no
            // destructors run twice. `EXIT_SUCCESS` is 0 on this host.
            unsafe { libc::_exit(0) };
        }
        if reap {
            let mut status: c_int = 0;
            loop {
                // SAFETY: a live `pid_t` and a live out-parameter.
                let done = unsafe { libc::waitpid(pid, &raw mut status, 0) };
                if done == pid {
                    break;
                }
                assert_eq!(
                    crate::errno::get(),
                    libc::EINTR,
                    "waitpid on our own child failed for a reason other than EINTR"
                );
            }
        }
        pid
    }

    /// This process's own pid: a pid that is certainly still running, which is
    /// the registration a test makes when it wants the change *rejected* before
    /// anything is opened rather than a race against a child.
    fn self_pid() -> i32 {
        // SAFETY: `getpid` takes no arguments and cannot fail.
        unsafe { libc::getpid() }
    }

    /// Chromium's `EV_SET(&change, pid, EVFILT_PROC, EV_ADD, NOTE_EXIT, 0, NULL)`,
    /// word for word. All three of the framework's registrations are these nine
    /// instructions, and all three of them call `_kevent`.
    fn watch_proc(pid: i32, flags: u16) -> Kevent {
        Kevent {
            ident: pid as u32 as usize,
            filter: EVFILT_PROC,
            flags,
            fflags: NOTE_EXIT,
            data: 0,
            udata: 0,
        }
    }

    /// Apply one change and answer with the number it failed with, which for
    /// this module is a `c_int` Darwin `errno` rather than a `Result`.
    ///
    /// **No room for events, deliberately.** A change that succeeds is followed
    /// by a collection in the same call, and a `pidfd` for a child that has
    /// *already* exited is readable - so with room for one event this would
    /// answer "delivered an event" where the caller asked "was this change
    /// applied", which is the wrong question. `nevents == 0` is also what the
    /// framework's own registration passes: `kevent(kq, &change, 1, NULL, 0, NULL)`.
    fn apply_one(queue: c_int, change: &Kevent) -> Result<(), i32> {
        // SAFETY: one change, no room for events, and no timeout.
        let n = unsafe {
            kevent(
                queue,
                std::ptr::from_ref(change),
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        match n {
            0 => Ok(()),
            _ => Err(crate::errno::get()),
        }
    }

    /// Wait on a queue for up to a second, answering the events it reports.
    fn collect(queue: c_int, room: usize) -> Vec<Kevent> {
        let mut got = vec![Kevent::default(); room];
        // SAFETY: no changes, `room` event slots and a one-second timeout.
        let n = unsafe {
            kevent(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                room as c_int,
                &raw const SECOND,
            )
        };
        assert!(n >= 0, "the wait failed: {}", crate::errno::get());
        got.truncate(n as usize);
        got
    }

    /// A pipe, as the two descriptors a readiness test needs.
    fn pipe() -> (c_int, c_int) {
        let mut ends = [0 as c_int; 2];
        // SAFETY: a live array of two `int`.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        (ends[0], ends[1])
    }

    /// The case the client needs: a descriptor is registered, nothing is ready,
    /// a byte arrives, and the *same* registration reports how much there is.
    #[test]
    fn a_readable_descriptor_is_reported_with_the_bytes_waiting() {
        let queue = kqueue();
        assert!(queue >= 0);
        let (read_end, write_end) = pipe();
        let add = Kevent {
            ident: read_end as usize,
            filter: EVFILT_READ,
            flags: EV_ADD,
            udata: 0xbeef,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 4];
        // SAFETY: one change and room for four events.
        unsafe {
            assert_eq!(
                kevent(
                    queue,
                    &raw const add,
                    1,
                    got.as_mut_ptr(),
                    4,
                    &raw const NO_WAIT
                ),
                0,
                "nothing is ready yet"
            );
            assert_eq!(libc::write(write_end, c"hello".as_ptr().cast(), 5), 5);
            assert_eq!(
                kevent(
                    queue,
                    std::ptr::null(),
                    0,
                    got.as_mut_ptr(),
                    4,
                    &raw const NO_WAIT
                ),
                1
            );
            libc::close(read_end);
            libc::close(write_end);
            libc::close(queue);
        }
        assert_eq!(got[0].ident, read_end as usize);
        assert_eq!(got[0].filter, EVFILT_READ);
        assert_eq!(got[0].udata, 0xbeef);
        assert_eq!(got[0].data, 5, "five bytes are waiting");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_queue_descriptor_is_readable_when_a_watched_descriptor_is_ready() {
        let queue = kqueue();
        let (read_end, write_end) = pipe();
        let add = Kevent {
            ident: read_end as usize,
            filter: EVFILT_READ,
            flags: EV_ADD,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: the descriptors and event buffers belong to this test.
        unsafe {
            assert_eq!(
                kevent(
                    queue,
                    &raw const add,
                    1,
                    got.as_mut_ptr(),
                    1,
                    &raw const NO_WAIT
                ),
                0
            );
            assert_eq!(libc::write(write_end, c"x".as_ptr().cast(), 1), 1);
            let mut watched = libc::pollfd {
                fd: queue,
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(libc::poll(&raw mut watched, 1, 0), 1);
            assert_ne!(watched.revents & libc::POLLIN, 0);
            assert_eq!(
                kevent(
                    queue,
                    std::ptr::null(),
                    0,
                    got.as_mut_ptr(),
                    1,
                    &raw const NO_WAIT
                ),
                1
            );
            libc::close(read_end);
            libc::close(write_end);
            libc::close(queue);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_epoll_keeps_regular_files_on_the_poll_compatibility_path() {
        use std::os::fd::AsRawFd;

        let queue = kqueue();
        let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/src/kqueue.rs"))
            .expect("this Rust source file");
        let add = Kevent {
            ident: file.as_raw_fd() as usize,
            filter: EVFILT_READ,
            flags: EV_ADD,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: one valid change and one event slot.
        let n = unsafe {
            kevent(
                queue,
                &raw const add,
                1,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 1);
        assert_eq!(got[0].ident, file.as_raw_fd() as usize);
        // SAFETY: this test's queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// `EVFILT_USER` is what a program wakes its own I/O thread with, and the
    /// trigger has to survive being set from another thread while `kevent` is
    /// blocked with no timeout.
    #[test]
    fn a_user_event_wakes_a_wait_that_had_no_timeout() {
        let queue = kqueue();
        let add = Kevent {
            ident: 42,
            filter: EVFILT_USER,
            flags: EV_ADD | EV_ENABLE,
            udata: 7,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: one change and room for one event.
        unsafe {
            assert_eq!(
                kevent(
                    queue,
                    &raw const add,
                    1,
                    got.as_mut_ptr(),
                    1,
                    &raw const NO_WAIT
                ),
                0
            );
        }
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            let fire = Kevent {
                ident: 42,
                filter: EVFILT_USER,
                fflags: NOTE_TRIGGER,
                ..Kevent::default()
            };
            // SAFETY: one change and no room for events, which applies it and
            // returns.
            unsafe {
                kevent(
                    queue,
                    &raw const fire,
                    1,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                )
            }
        });
        // SAFETY: no changes, room for one event, and a null timeout - which is
        // the wait the trigger has to break.
        let n = unsafe {
            kevent(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                std::ptr::null(),
            )
        };
        assert_eq!(trigger.join().expect("the trigger thread"), 0);
        assert_eq!(n, 1);
        assert_eq!(got[0].ident, 42);
        assert_eq!(got[0].udata, 7);
        assert_eq!(
            got[0].fflags & NOTE_TRIGGER,
            0,
            "the caller's request is not handed back as a property of the event"
        );
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    /// Every filter this module refuses, refused **by name**.
    ///
    /// The number alone is what the guest gets; the *name* is what the log gets,
    /// and the log is the only place a missing capability is visible at all -
    /// `base::WaitForSingleNonChildProcess` sends everything but `ESRCH` to a
    /// `DLOG`, which a release build compiles out. So the census is asserted
    /// here rather than left in a message nobody checks: every one of these
    /// filters is refused, and every one of them *has* a name, because a filter
    /// this file has never heard of keeps its number and says so.
    #[test]
    fn every_refused_filter_is_refused_and_has_a_name() {
        let queue = kqueue();
        // The set `refused_filter` claims, from `<sys/event.h>`.
        for (filter, number) in [
            (-3, "EVFILT_AIO"),
            (-4, "EVFILT_VNODE"),
            (-6, "EVFILT_SIGNAL"),
            (-7, "EVFILT_TIMER"),
            (-8, "EVFILT_MACHPORT"),
            (-9, "EVFILT_FS"),
            (-11, "EVFILT_DEVICE"),
            (-12, "EVFILT_VM"),
            (-13, "EVFILT_WORKLOOP"),
            (-14, "EVFILT_USER_EVENT"),
            (-15, "EVFILT_EXCEPT"),
        ] {
            assert_eq!(filter_name(filter), number, "the census is complete");
            let add = Kevent {
                ident: 1,
                filter,
                flags: EV_ADD,
                ..Kevent::default()
            };
            assert_eq!(
                apply_one(queue, &add),
                Err(crate::errno::from_host(libc::EINVAL)),
                "{number} is refused"
            );
        }
        // And a filter that is not a name at all keeps its number rather than
        // borrowing one, which is what `filter_name`'s last arm is for.
        assert_eq!(filter_name(-99), "a filter kqueue does not define");
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    /// The registration, the wait, and the three fields the framework reads -
    /// through `_kevent`, which is the only way the framework reaches this
    /// filter at all.
    ///
    /// The shape is `base::WaitForSingleNonChildProcess`
    /// (`base/process/process_posix.cc`) and
    /// `mac_relauncher::RelauncherSynchronizeWithParent`
    /// (`chrome/browser/mac/relauncher.mm`): register with `EV_ADD` and
    /// `NOTE_EXIT`, then wait. Both read `filter`, `fflags` and `ident` and
    /// neither reads `flags` or `data`.
    #[test]
    fn a_process_that_exits_reports_the_filter_fflags_and_ident_it_was_given() {
        let queue = kqueue();
        let pid = child_that_exits(/* reap */ false);
        assert_eq!(apply_one(queue, &watch_proc(pid, EV_ADD)), Ok(()));

        let got = collect(queue, 1);
        assert_eq!(got.len(), 1, "the child exited, so there is one event");
        // Exactly the three fields both callers read.
        assert_eq!(got[0].filter, EVFILT_PROC);
        assert_eq!(got[0].fflags, NOTE_EXIT);
        assert_eq!(got[0].ident, pid as u32 as usize);

        // The whole `flags` word, because it is known rather than guessed:
        // `struct knote` has `#define kn_flags kn_kevent.kei_flags`, so the
        // knote's `flags` *is* the field `kevent_register` copied the caller's
        // change into - which is how the caller's own `EV_ADD` arrives - and
        // `filt_procattach` and `filt_procevent` then add `EV_CLEAR` and
        // `EV_EOF | EV_ONESHOT` to that same field. `EV_EOF` is `0x8000` of
        // `flags`; `NOTE_EXIT` is `0x80000000` of `fflags`; they are the two
        // halves of one value in two different fields, and the guest's test is on
        // the one in `fflags`.
        assert_eq!(
            got[0].flags,
            EV_ADD | EV_ONESHOT | EV_CLEAR | EV_EOF,
            "the caller's EV_ADD with the kernel's three additions"
        );
        assert_eq!(got[0].flags & EV_ERROR, 0, "and this is not an error event");
        // `data` is zero, which is XNU's `kn_hook32` for a registration that did
        // not ask for NOTE_EXITSTATUS or NOTE_EXIT_DETAIL - both refused. A
        // non-zero here would be a fabricated exit status.
        assert_eq!(got[0].data, 0);

        // One-shot. `filt_procevent` sets `EV_ONESHOT` on `NOTE_EXIT` and the
        // knote was attached to a `struct proc` that no longer exists, so there
        // is nothing left to wait on. This is the assertion that a `pidfd` makes
        // necessary rather than optional: it **stays readable** for as long as the
        // descriptor lives, so a registration left armed would report the same
        // exit on every call.
        assert!(collect(queue, 1).is_empty(), "and it is not a second event");
        assert_eq!(
            apply_one(queue, &watch_proc(pid, EV_DELETE)),
            Err(libc::ENOENT),
            "and the registration is gone rather than re-armable"
        );

        // SAFETY: this test's own queue descriptor, and a child of ours.
        unsafe {
            libc::close(queue);
            let mut status: c_int = 0;
            libc::waitpid(pid, &raw mut status, 0);
        }
    }

    /// `EV_DELETE` removes the registration and closes the `pidfd`, and
    /// cancelling while another thread waits does not close it underneath.
    #[test]
    fn deleting_a_proc_registration_closes_its_pidfd() {
        let queue = kqueue();
        let pid = child_that_exits(/* reap */ false);
        assert_eq!(apply_one(queue, &watch_proc(pid, EV_ADD)), Ok(()));

        // The descriptor the registration is holding, so that "it was closed" is
        // a measurement rather than an inference.
        let fd = {
            let state = queues()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&queue)
                .cloned()
                .expect("the queue is in this module's table");
            let state = state.lock().unwrap_or_else(|e| e.into_inner());
            state.registrations[&(pid as u32 as usize, EVFILT_PROC)]
                .proc
                .as_ref()
                .expect("an EVFILT_PROC registration holds its pidfd")
                .raw_fd()
        };

        assert_eq!(apply_one(queue, &watch_proc(pid, EV_DELETE)), Ok(()));
        // Dropping the queue's `Arc` was the last reference, so the descriptor is
        // closed: `F_GETFD` is `EBADF` on a number that is not open, and the
        // number has not been reused because nothing else in this process opened
        // a descriptor since.
        // SAFETY: `F_GETFD` reads no argument and writes no memory.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        assert_eq!(crate::errno::get(), libc::EBADF);
        // And the second delete is ENOENT, the check that catches a delete of a
        // registration that was never made.
        assert_eq!(apply_one(queue, &watch_proc(pid, EV_DELETE)), Err(libc::ENOENT));
        // Which is also what a change that only *re-states* a registration gets
        // when there is none: `kevent_register` finds no knote and was not given
        // `EV_ADD`, so the answer is `ENOENT` and not "now watching". It has to
        // be asked with `EV_RECEIPT` and room for the receipt, because a
        // receipt *replaces* the return value: `EV_RECEIPT` asks for the
        // outcome of every change rather than for readiness, and it asks whether
        // or not the change failed.
        let receipt = Kevent {
            ident: pid as u32 as usize,
            filter: EVFILT_PROC,
            flags: EV_RECEIPT,
            fflags: NOTE_EXIT,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: one change, room for the one receipt, and a zero timeout.
        let n = unsafe {
            kevent(
                queue,
                std::ptr::from_ref(&receipt),
                1,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 1, "EV_RECEIPT answers with a receipt rather than a return value");
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        assert_eq!(
            got[0].data as i32,
            crate::errno::from_host(libc::ENOENT),
            "a receipt for a registration that was never made is ENOENT, not a watch"
        );

        // SAFETY: this test's own queue descriptor, and a child of ours.
        unsafe {
            libc::close(queue);
            let mut status: c_int = 0;
            libc::waitpid(pid, &raw mut status, 0);
        }
    }

    /// A pid that is not there is `ESRCH`, which is the reference's own answer
    /// and the one both of the framework's callers act on: they read `ESRCH` as
    /// "the process is not there, so it is dead" and return.
    #[test]
    fn a_pid_that_does_not_exist_is_esrch_which_the_callers_read_as_dead() {
        let queue = kqueue();
        // A child that has exited *and* been reaped: the pid is free, so
        // `pidfd_open` cannot open it. A `pidfd` on a *zombie* succeeds - the
        // zombie is still a process - which is why this one reaps.
        let pid = child_that_exits(/* reap */ true);
        assert_eq!(
            apply_one(queue, &watch_proc(pid, EV_ADD)),
            Err(libc::ESRCH),
            "filt_procattach answers ESRCH for an ident proc_find cannot find"
        );
        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// `EV_DISABLE` takes a registration out of the wait and `EV_ENABLE` puts it
    /// back, and neither disturbs the `pidfd`: the process has not stopped
    /// existing while a knote was disabled, and the descriptor is what the knote
    /// *is*.
    #[test]
    fn a_disabled_proc_registration_is_not_polled_and_comes_back_intact() {
        let queue = kqueue();
        let pid = child_that_exits(/* reap */ false);
        assert_eq!(apply_one(queue, &watch_proc(pid, EV_ADD | EV_DISABLE)), Ok(()));

        // The child has exited, but the registration is disabled, so nothing is
        // reported. This is a real wait, not a collection: a `poll` on the
        // disabled registration's `pidfd` would have been the bug.
        let mut got = [Kevent::default(); 1];
        // SAFETY: no changes, one event slot, and a 200 ms timeout.
        let n = unsafe {
            kevent(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                &raw const A_FIFTH,
            )
        };
        assert_eq!(n, 0, "a disabled registration is not reported");

        // `EV_ENABLE` with no `EV_ADD`, which is the form that changes an
        // existing registration rather than creating one. A `NOTE_EXIT` fflags is
        // not part of that form, so the fflags is zero - and `proc_change` says a
        // zero fflags is `NOTE_EXIT` and nothing else, so it is served.
        let mut enable = watch_proc(pid, EV_ENABLE);
        enable.fflags = 0;
        assert_eq!(apply_one(queue, &enable), Ok(()));

        let got = collect(queue, 1);
        assert_eq!(got.len(), 1, "and now it is reported");
        assert_eq!(got[0].ident, pid as u32 as usize);

        // SAFETY: this test's own queue descriptor, and a child of ours.
        unsafe {
            libc::close(queue);
            let mut status: c_int = 0;
            libc::waitpid(pid, &raw mut status, 0);
        }
    }

    /// The `fflags` a `pidfd` cannot report, and the two it must refuse with the
    /// *reference's* numbers rather than its own.
    #[test]
    fn proc_notes_a_pidfd_cannot_report_are_refused_by_number() {
        let queue = kqueue();
        // `NOTE_EXITSTATUS` is the one `<sys/event.h>` documents as putting the
        // wait(2) status word in `data`. The only way to learn it is to reap, and
        // reaping is what the framework's own `waitpid` calls collect - so this is
        // refused rather than answered with a zero that would read as "exited
        // with status 0".
        let mut status = watch_proc(self_pid(), EV_ADD);
        status.fflags = NOTE_EXIT | NOTE_EXITSTATUS;
        assert_eq!(apply_one(queue, &status), Err(libc::EINVAL));

        // The jetsam / CSKILLED / decrypt-fail reasons: fields of a macOS
        // `struct proc` that a Linux one does not have.
        let mut detail = watch_proc(self_pid(), EV_ADD);
        detail.fflags = 0x0200_0000; // NOTE_EXIT_DETAIL
        assert_eq!(apply_one(queue, &detail), Err(libc::EINVAL));

        // The process lifecycle. `NOTE_FORK` is 0x40000000 and `NOTE_EXEC` is
        // 0x20000000; neither is something a descriptor reports.
        for note in [0x4000_0000u32, 0x2000_0000, 0x1000_0000, 0x0000_0010] {
            let mut lifecycle = watch_proc(self_pid(), EV_ADD);
            lifecycle.fflags = NOTE_EXIT | note;
            assert_eq!(
                apply_one(queue, &lifecycle),
                Err(libc::EINVAL),
                "fflags {note:#x} should be refused"
            );
        }

        // The three "follow it across fork" notes are refused by the reference
        // itself, with `ENOTSUP`, *before* it looks at the pid - and before the
        // pid here is even a valid one, which is the point: the order is XNU's.
        for note in [0x0000_0001u32, 0x0000_0002, 0x0000_0004] {
            let mut track = watch_proc(self_pid(), EV_ADD);
            track.fflags = note;
            assert_eq!(
                apply_one(queue, &track),
                Err(libc::ENOTSUP),
                "filt_procattach answers ENOTSUP for {note:#x}, and the guest sees that number"
            );
        }

        // `EV_EOF` and `EV_ERROR` are what the kernel *returns*; passing them in
        // a change is refused, as it is for every other filter.
        assert_eq!(
            apply_one(queue, &watch_proc(self_pid(), EV_ADD | EV_EOF)),
            Err(libc::EINVAL)
        );

        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A `pidfd` stays readable forever, so an event that did not fit in
    /// `nevents` must not be lost. This is the trap: a `timerfd` can be read to
    /// clear it, and a `pidfd` has nothing to read.
    #[test]
    fn a_proc_event_that_did_not_fit_is_reported_by_the_next_wait() {
        let queue = kqueue();
        // Two children, so two registrations, and the first to exit is whichever
        // the kernel says - this does not depend on which.
        let first = child_that_exits(/* reap */ false);
        let second = child_that_exits(/* reap */ false);
        assert_eq!(apply_one(queue, &watch_proc(first, EV_ADD)), Ok(()));
        assert_eq!(apply_one(queue, &watch_proc(second, EV_ADD)), Ok(()));

        // One slot for two events: one is delivered and the other is not.
        let got = collect(queue, 1);
        assert_eq!(got.len(), 1);
        let delivered = got[0].ident;

        // The other is still there, and the next wait says so. If the delivery
        // had dropped every ready registration rather than the delivered one, the
        // fact that a second process exited would be gone with it.
        let got = collect(queue, 1);
        assert_eq!(got.len(), 1, "the event that did not fit is still pending");
        assert_ne!(got[0].ident, delivered);

        // SAFETY: this test's own queue descriptor, and two children of ours.
        unsafe {
            libc::close(queue);
            for pid in [first, second] {
                let mut status: c_int = 0;
                libc::waitpid(pid, &raw mut status, 0);
            }
        }
    }

    /// **A pid is not a descriptor**, and the three places in this file that
    /// depend on that are asserted directly.
    ///
    /// This is not a nicety. Every other filter's `ident` is a descriptor
    /// number, so the file folds a queue's interest together by descriptor and
    /// finds a ready descriptor's registrations by comparing it to `ident`. If a
    /// proc registration were folded in that way, a pid that happened to equal
    /// *another* registration's descriptor number would be reported as that
    /// registration's readiness: an event nobody asked for, about a descriptor
    /// nobody named, delivered to a caller that would act on it. Nothing about
    /// that fails; it answers.
    #[test]
    fn a_pid_is_not_a_descriptor() {
        // This process: alive, so the registration is legitimate and stays armed,
        // and its pid is a real number that is also a legal `c_int`.
        let pid = self_pid();
        let queue = kqueue();
        assert_eq!(apply_one(queue, &watch_proc(pid, EV_ADD)), Ok(()));
        let state = queues()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&queue)
            .cloned()
            .expect("the queue is in this module's table");
        let state = state.lock().unwrap_or_else(|e| e.into_inner());
        let registration = &state.registrations[&(pid as u32 as usize, EVFILT_PROC)];

        // The descriptor it waits on is the `pidfd`, and *not* the pid.
        let fd = descriptor_of(registration).expect("a pidfd");
        assert_ne!(fd, pid, "the pid is not the descriptor");
        assert!(
            unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
            "and the descriptor is open"
        );
        // It is found by its `pidfd` and **not** by its pid.
        assert!(registration.watches(fd));
        assert!(
            !registration.watches(pid),
            "a pid must not be mistaken for a descriptor, in either direction"
        );
        // And the pid contributes no interest to the descriptor that happens to
        // have the same number, which is the other half of the same mistake.
        assert_eq!(
            interest_for(&state.registrations, pid),
            0,
            "no interest is registered on the pid"
        );
        // Its own descriptor carries the `poll` interest.
        assert_eq!(interest_for(&state.registrations, fd), libc::POLLIN);
        drop(state);
        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A filter this module cannot answer is refused, and `EV_RECEIPT` is how
    /// the caller finds out which change it was.
    ///
    /// **This test used to name `EVFILT_VNODE` as its example, and that was a
    /// false pin.** It asserted, in the name of a passing test, that the one
    /// refused filter the framework *registers and waits on* could not be
    /// answered - which was the reason the file watcher was dead, stated as if it
    /// were a property of the platform. `EVFILT_VNODE` is answered now, on a real
    /// `inotify` watch, and the tests below are what say so. `EVFILT_FS` is used
    /// here instead because it is still refused, and it is refused for the reason
    /// given: it names a *mount*, and a mount is not an object with a pathname
    /// this host can watch.
    #[test]
    fn an_unsupported_filter_is_refused_by_number() {
        let queue = kqueue();
        const EVFILT_FS: i16 = -9;
        let add = Kevent {
            ident: 1,
            filter: EVFILT_FS,
            flags: EV_ADD | EV_RECEIPT,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: one change and room for one event.
        let n = unsafe {
            kevent(
                queue,
                &raw const add,
                1,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 1);
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        assert_eq!(got[0].data as i32, crate::errno::from_host(libc::EINVAL));
        // And without `EV_RECEIPT` the whole call fails.
        let add = Kevent {
            flags: EV_ADD,
            ..add
        };
        assert_eq!(
            unsafe {
                kevent(
                    queue,
                    &raw const add,
                    1,
                    got.as_mut_ptr(),
                    1,
                    &raw const NO_WAIT,
                )
            },
            -1
        );
        // SAFETY: this test's own descriptor.
        unsafe { libc::close(queue) };
    }

    /// A descriptor that is not a queue is `EBADF` rather than a queue with
    /// nothing in it.
    #[test]
    fn a_descriptor_that_is_not_a_queue_is_refused() {
        let mut got = [Kevent::default(); 1];
        // SAFETY: standard input is open and is not a kqueue.
        let n = unsafe {
            kevent(
                0,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, -1);
        assert_eq!(crate::errno::get(), libc::EBADF);
    }

    // -----------------------------------------------------------------------
    // `EVFILT_VNODE`
    //
    // Everything below is arranged so that it **fails if the filter is not
    // implemented**: each one creates a real file or directory, registers it the
    // way the framework's two `FilePathWatcherKQueue` classes do, changes
    // something, and asserts the delivered `fflags` and `ident`. With the filter
    // refused, the registration's receipt carries `EINVAL` and every subsequent
    // collection returns nothing.
    // -----------------------------------------------------------------------

    /// The seven public `EVFILT_VNODE` notes, quoted from XNU's `bsd/sys/event.h`
    /// rather than imported, because the *set* `0x6f` is the thing the framework
    /// submits and what the narrowing is measured against.
    const VNOTE_DELETE: u32 = 0x0000_0001;
    const VNOTE_WRITE: u32 = 0x0000_0002;
    const VNOTE_EXTEND: u32 = 0x0000_0004;
    const VNOTE_ATTRIB: u32 = 0x0000_0008;
    const VNOTE_LINK: u32 = 0x0000_0010;
    const VNOTE_RENAME: u32 = 0x0000_0020;
    const VNOTE_REVOKE: u32 = 0x0000_0040;

    /// `movabs rax,0x6f0061fffc` - the top eight bytes, which is what the
    /// framework's six registrations in the image actually submit.
    const ALL_SEVEN: u32 =
        VNOTE_DELETE | VNOTE_WRITE | VNOTE_EXTEND | VNOTE_ATTRIB | VNOTE_LINK | VNOTE_RENAME
            | VNOTE_REVOKE;

    /// A real directory to run a file-system test in.
    ///
    /// `std::env::temp_dir()` and the pid, which is the convention every other
    /// test in this crate uses, and the pid is there because two suites can be
    /// running at once on this host. `temp_dir` reads `TMPDIR`, and the run script
    /// points that at `/home/frolleks/tmp` - which matters, because `/tmp` here
    /// is a tmpfs backed by RAM.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("r3-kq-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    /// The descriptor the framework's `FilePathWatcherKQueue` registers: what its
    /// `FileDescriptorForPath` gets from `open(path, O_EVTONLY)`.
    ///
    /// **`O_PATH` and not `O_RDONLY`, because `O_PATH` is what this runtime
    /// answers `O_EVTONLY` with** ([`crate::fcntl`], and the reason it is the
    /// same capability rather than a stand-in: `read` is `EBADF`, `fstat` and
    /// `close` work, a missing path is `ENOENT`). A test that opened the file for
    /// reading instead would be testing a capability the guest never has, and
    /// would pass on a path that could not have been reached from the guest.
    fn open_evtonly(path: &std::path::Path) -> c_int {
        let mut name = path.as_os_str().as_encoded_bytes().to_vec();
        name.push(0);
        // `O_PATH | O_NOFOLLOW | O_CLOEXEC` = 0x200000 | 0x20000 | 0x80000.
        // SAFETY: `name` is NUL-terminated and the pointer is valid for the call.
        let fd = unsafe {
            libc::open(
                name.as_ptr().cast::<libc::c_char>(),
                0x200000 | 0x20000 | 0x80000,
            )
        };
        assert!(fd >= 0, "open {}: {}", path.display(), crate::errno::get());
        fd
    }

    /// Chromium's `EV_SET(&event, fd, EVFILT_VNODE, EV_ADD|EV_CLEAR|EV_RECEIPT,
    /// <all seven notes>, 0, data)`, word for word. The `udata` is distinctive so
    /// a delivered event can be traced back to the registration that asked for
    /// it rather than to a number that happens to match.
    fn watch_vnode(fd: c_int, fflags: u32, udata: usize) -> Kevent {
        Kevent {
            ident: fd as usize,
            filter: EVFILT_VNODE,
            flags: EV_ADD | EV_CLEAR | EV_RECEIPT,
            fflags,
            data: 0,
            udata,
        }
    }

    /// Register `fd` and answer the receipt.
    ///
    /// **The receipt is the whole question**, and it is asserted rather than
    /// assumed: `FilePathWatcherKQueue::AreKeventValuesValid` is a two-instruction
    /// test in the guest - `test BYTE PTR [rbx+r15*1+0xb],0x40` then
    /// `cmp QWORD PTR [r15+0x10],0x0` at `0x5d9e505` and `0x5d9e510`, and its
    /// `__FILE__`-bracketed twin at `0x3a9a6d5`/`0x3a9a6e0` - so it accepts a
    /// receipt whose `data` is zero and rejects every other, whatever produced it.
    /// `data == 0` here is a true statement about a change that was carried out,
    /// and it is the single bit that makes `Watch()` return true instead of
    /// false.
    fn register_vnode(queue: c_int, fd: c_int, fflags: u32, udata: usize) {
        let add = watch_vnode(fd, fflags, udata);
        let mut got = [Kevent::default(); 1];
        // SAFETY: one change and room for the one receipt.
        let n = unsafe {
            kevent(
                queue,
                &raw const add,
                1,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 1, "a receipt answers with an event, not a return value");
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR, "a receipt is an EV_ERROR");
        assert_eq!(
            got[0].data, 0,
            "the registration was refused with {:?}; the watcher would never fire",
            crate::errno::from_host(got[0].data as i32)
        );
        // The caller's own words are echoed, which is how it tells *which* change
        // this receipt is about.
        assert_eq!(got[0].ident, fd as usize);
        assert_eq!(got[0].filter, EVFILT_VNODE);
        assert_eq!(got[0].udata, udata);
    }

    /// Collect with a short timeout, for the assertions whose point is that
    /// *nothing* arrives.
    fn collect_briefly(queue: c_int, room: usize) -> Vec<Kevent> {
        let brief = libc::timespec {
            tv_sec: 0,
            tv_nsec: 50_000_000,
        };
        let mut got = vec![Kevent::default(); room];
        // SAFETY: no changes, `room` slots, and a 50ms timeout.
        let n = unsafe {
            kevent(
                queue,
                std::ptr::null(),
                0,
                got.as_mut_ptr(),
                room as c_int,
                &raw const brief,
            )
        };
        assert!(n >= 0, "the wait failed: {}", crate::errno::get());
        got.truncate(n as usize);
        got
    }

    /// Every event a watcher collected, with the `fflags` words ORed together -
    /// and the assertion that no *unreportable* note ever appears in one.
    ///
    /// **The subset assertion is the one that makes this filter honest.** A
    /// change list is a filter, not a demand: `0x6f` asks for seven notes and
    /// this host can report five, so a delivered `fflags` carrying `NOTE_EXTEND`
    /// or `NOTE_LINK` would be a claim that something was observed when it was
    /// not. The measurement that dropping them costs this guest nothing is in
    /// [`super::mach::kevent64::VNODE_DROPPED_BY_THE_GUEST`]; the measurement
    /// that they are never *asked for* is these tests.
    fn notes_of(events: &[Kevent], room: usize) -> u32 {
        assert!(
            events.len() <= room,
            "a zero-timeout collection cannot report more than its room"
        );
        let mut word = 0;
        for event in events {
            assert_eq!(event.filter, EVFILT_VNODE);
            assert_eq!(
                event.fflags & (VNOTE_EXTEND | VNOTE_LINK),
                0,
                "NOTE_EXTEND and NOTE_LINK are refused by name, so they must never appear in a \
                 delivered fflags - a note this host cannot observe must never be reported"
            );
            word |= event.fflags;
        }
        word
    }

    /// The case the client needs: a watched file is written, and the write is
    /// reported as `NOTE_WRITE` on the caller's own `ident`.
    ///
    /// **This is the test that fails if the filter is refused.** With `EVFILT_VNODE`
    /// refused, `register_vnode`'s receipt carries `EINVAL` and this panics there;
    /// even if that were papered over, the collection below would find nothing.
    #[test]
    fn a_watched_file_that_is_written_reports_note_write_on_its_own_ident() {
        let dir = scratch("kq-vnode-write");
        let file = dir.join("watched");
        std::fs::write(&file, b"before").unwrap();
        let fd = open_evtonly(&file);
        let queue = kqueue();
        register_vnode(queue, fd, ALL_SEVEN, 0x5150);

        // Append, so the change is a `write(2)` and nothing else: a truncation
        // would also be a `VNOP_SETATTR`, and the two are not being told apart
        // here.
        {
            use std::io::Write;
            let mut handle = std::fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .unwrap();
            handle.write_all(b"after").unwrap();
        }

        let got = collect(queue, 4);
        let notes = notes_of(&got, 4);
        assert!(
            notes & VNOTE_WRITE != 0,
            "a write to a watched file is NOTE_WRITE ({notes:#x}), and nothing else was reported"
        );
        // The delivered `ident` is the descriptor the caller registered, which is
        // what `OnKQueueReadable` matches against: an event carrying anything
        // else is an event about an object nobody named.
        assert_eq!(got[0].ident, fd as usize);
        assert_eq!(got[0].udata, 0x5150);
        assert_eq!(
            got[0].data, 0,
            "filt_vnprocess delivers data = 0 for every note it can raise"
        );

        // SAFETY: this test's own two descriptors.
        unsafe { libc::close(fd) };
        unsafe { libc::close(queue) };
    }

    /// A rename is `NOTE_RENAME` and a delete is `NOTE_DELETE` - **not each
    /// other's opposite number**, which is the wrong answer a file watcher cannot
    /// detect.
    #[test]
    fn a_rename_is_note_rename_and_a_delete_is_note_delete_and_not_the_reverse() {
        let dir = scratch("kq-vnode-rename-delete");
        let file = dir.join("watched");
        let moved = dir.join("moved");
        std::fs::write(&file, b"x").unwrap();
        let fd = open_evtonly(&file);
        let queue = kqueue();
        register_vnode(queue, fd, ALL_SEVEN, 0xDE1E);

        std::fs::rename(&file, &moved).unwrap();
        let renamed = collect(queue, 4);
        let notes = notes_of(&renamed, 4);
        assert!(
            notes & VNOTE_RENAME != 0,
            "a rename of a watched file is NOTE_RENAME ({notes:#x})"
        );
        assert_eq!(
            notes & VNOTE_DELETE,
            0,
            "a rename is not a delete: reporting NOTE_DELETE for a rename would close the \
             descriptor the framework is still using, and the guest cannot tell the two apart"
        );

        std::fs::remove_file(&moved).unwrap();
        let deleted = collect(queue, 4);
        let notes = notes_of(&deleted, 4);
        assert!(
            notes & VNOTE_DELETE != 0,
            "an unlink of a watched file is NOTE_DELETE ({notes:#x})"
        );
        assert_eq!(
            notes & VNOTE_RENAME,
            0,
            "a delete is not a rename"
        );

        // SAFETY: this test's own two descriptors.
        unsafe { libc::close(fd) };
        unsafe { libc::close(queue) };
    }

    /// A delete is reported **once**.
    ///
    /// Measured, and this is why it has to be once: an `unlink` of a watched file
    /// with its descriptor still open delivers `IN_ATTRIB` immediately and
    /// `IN_DELETE_SELF` when the last reference goes - and
    /// `HandleDeleteOrMoveChange` closes the descriptor as soon as it has seen the
    /// first, so on this guest the second event is *guaranteed* to arrive.
    /// XNU posts `NOTE_DELETE` once, from `VNOP_REMOVE`.
    #[test]
    fn a_delete_is_reported_once_and_not_again() {
        let dir = scratch("kq-vnode-delete-once");
        let file = dir.join("watched");
        std::fs::write(&file, b"x").unwrap();
        let fd = open_evtonly(&file);
        let queue = kqueue();
        register_vnode(queue, fd, ALL_SEVEN, 0x0D0E);

        std::fs::remove_file(&file).unwrap();
        let first = collect(queue, 4);
        assert!(
            notes_of(&first, 4) & VNOTE_DELETE != 0,
            "the unlink itself is NOTE_DELETE"
        );
        assert!(
            collect_briefly(queue, 4).is_empty(),
            "the fact was already reported; saying it again is a busy watcher"
        );

        // SAFETY: this test's own two descriptors.
        unsafe { libc::close(fd) };
        unsafe { libc::close(queue) };
    }

    /// A `chmod` is `NOTE_ATTRIB` - the one of `IN_ATTRIB`'s three causes that
    /// does have a note of its own.
    #[test]
    fn a_chmod_is_note_attrib() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("kq-vnode-attrib");
        let file = dir.join("watched");
        std::fs::write(&file, b"x").unwrap();
        let fd = open_evtonly(&file);
        let queue = kqueue();
        register_vnode(queue, fd, ALL_SEVEN, 0xA7B7);

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let got = collect(queue, 4);
        assert!(
            notes_of(&got, 4) & VNOTE_ATTRIB != 0,
            "a mode change is a VNOP_SETATTR and XNU posts NOTE_ATTRIB for it"
        );

        // SAFETY: this test's own two descriptors.
        unsafe { libc::close(fd) };
        unsafe { libc::close(queue) };
    }

    /// **A link count going *up* reports nothing at all**, and this is the test
    /// that keeps a refusal from turning into a plausible lie.
    ///
    /// `inotify` reports a hard link as `IN_ATTRIB` - the *same bit* as a mode,
    /// ownership or timestamp change - and nothing in the event says which. XNU
    /// posts `NOTE_LINK` and **nothing else** for `VNOP_LINK`, so mapping this to
    /// `NOTE_ATTRIB` would be a claim macOS does not make: it would tell the
    /// framework that the file's *attributes* changed when only its *link count*
    /// did. So the count is read and compared, and a change in it is reported as
    /// no note at all, because the note that says so (`NOTE_LINK`) is refused by
    /// name.
    ///
    /// **The positive control is inside the test**, and it is what makes the
    /// negative assertion mean anything: the same watch is still armed afterwards
    /// and still reports a write. Without it, "nothing arrived" would be
    /// indistinguishable from "the watch is dead", and the test would pass for
    /// entirely the wrong reason.
    #[test]
    fn a_hard_link_reports_no_note_because_note_link_is_refused() {
        let dir = scratch("kq-vnode-link");
        let file = dir.join("watched");
        let alias = dir.join("alias");
        std::fs::write(&file, b"x").unwrap();
        let fd = open_evtonly(&file);
        let queue = kqueue();
        register_vnode(queue, fd, ALL_SEVEN, 0x11AD);

        std::fs::hard_link(&file, &alias).unwrap();
        let got = collect_briefly(queue, 4);
        assert!(
            got.is_empty(),
            "a hard link raises IN_ATTRIB, which is the bit a metadata change uses, but XNU \
             posts NOTE_LINK and nothing else for VNOP_LINK. Reporting NOTE_ATTRIB here would \
             claim the file's attributes changed; got {:?}",
            got.iter().map(|event| event.fflags).collect::<Vec<_>>()
        );

        // The positive control: the watch is alive and still armed, so the
        // emptiness above was about the note and not about a dead registration.
        {
            use std::io::Write;
            let mut handle = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
            handle.write_all(b"y").unwrap();
        }
        let alive = collect(queue, 4);
        assert!(
            notes_of(&alive, 4) & VNOTE_WRITE != 0,
            "the same watch still reports a write, so it was never dead"
        );

        // SAFETY: this test's own two descriptors.
        unsafe { libc::close(fd) };
        unsafe { libc::close(queue) };
    }

    /// A watched **directory** whose contents change is `NOTE_WRITE` - and a
    /// child's *metadata* change is **not** reported on the parent.
    ///
    /// This is the other of the two wrong answers a file watcher cannot detect: a
    /// `NOTE_ATTRIB` on the parent would be a claim about a *different file* than
    /// the one that changed, and `HandleAttributesChange` acts on it by closing
    /// descriptors and re-opening the next path component. XNU raises a child's
    /// `NOTE_ATTRIB` on the child's own vnode, where the parent's registration has
    /// no knote at all.
    #[test]
    fn a_directory_watch_reports_a_create_as_note_write_and_a_childs_chmod_as_nothing() {
        let dir = scratch("kq-vnode-dir");
        let child = dir.join("child");
        std::fs::write(&child, b"x").unwrap();
        let fd = open_evtonly(&dir);
        let queue = kqueue();
        register_vnode(queue, fd, ALL_SEVEN, 0x8175);

        // A new entry inside the watched directory. `inotify` says `IN_CREATE`
        // *with a name*, and XNU raises `NOTE_WRITE` on the directory for it from
        // `VNOP_MKNOD`.
        std::fs::write(dir.join("created"), b"new").unwrap();
        let created = collect(queue, 8);
        let notes = notes_of(&created, 8);
        assert!(
            notes & VNOTE_WRITE != 0,
            "an entry appearing in a watched directory is a NOTE_WRITE on the directory ({notes:#x})"
        );
        assert_eq!(
            notes & VNOTE_ATTRIB,
            0,
            "creating an entry is not a metadata change to the directory itself"
        );

        // A *child's* mode change. `inotify` reports this on the parent as an
        // `IN_ATTRIB` **with a name**, and it must be dropped: XNU posts
        // NOTE_ATTRIB on the child's own vnode, and the parent's registration has
        // no knote there.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o600)).unwrap();
        let attrib = collect_briefly(queue, 8);
        assert!(
            notes_of(&attrib, 8) & VNOTE_ATTRIB == 0,
            "a child's metadata change is NOT the parent's NOTE_ATTRIB; claiming it would make \
             HandleAttributesChange close descriptors because of another file, and the guest \
             cannot tell that apart from a real one"
        );

        // The positive control again: the directory watch still fires.
        std::fs::write(dir.join("created-2"), b"new").unwrap();
        assert!(
            notes_of(&collect(queue, 8), 8) & VNOTE_WRITE != 0,
            "the directory watch is still armed"
        );

        // SAFETY: this test's own two descriptors.
        unsafe { libc::close(fd) };
        unsafe { libc::close(queue) };
    }

    /// A registration that asks for **only** notes this host cannot observe is
    /// **refused**, not accepted.
    ///
    /// `EINVAL`, and by name, because the alternative - a registration that
    /// succeeds, a queue that reports its descriptor readable, and no event that
    /// can ever arrive - is the one outcome this module is written to avoid.
    ///
    /// **Both answers are asserted, because they differ and the difference is
    /// XNU's.** Without `EV_RECEIPT` the refusal is `errno` and the call is `-1`;
    /// *with* `EV_RECEIPT` and **no room for events** the error has nowhere to go
    /// and the call answers `0` - the receipt is simply not delivered, which is
    /// what `kevent_register` does with a receipt it cannot write. That is why the
    /// first case here carries no `EV_RECEIPT`: with it, this test would be
    /// asserting the receipt path rather than the refusal.
    #[test]
    fn a_change_asking_only_for_unreportable_notes_is_refused() {
        let dir = scratch("kq-vnode-all-unreportable");
        let file = dir.join("watched");
        std::fs::write(&file, b"x").unwrap();
        let fd = open_evtonly(&file);
        let queue = kqueue();
        let add = Kevent {
            flags: EV_ADD,
            ..watch_vnode(fd, VNOTE_EXTEND | VNOTE_LINK, 0)
        };
        assert_eq!(
            apply_one(queue, &add),
            Err(crate::errno::from_host(libc::EINVAL)),
            "there is no filter left to register, so this is refused rather than accepted"
        );
        // And nothing is left armed behind it.
        assert!(
            collect_briefly(queue, 4).is_empty(),
            "a refused registration has armed nothing"
        );
        // The same change with room for its receipt says so in the receipt rather
        // than in `errno`, and still says *something*.
        let mut got = [Kevent::default(); 1];
        let receipt = watch_vnode(fd, VNOTE_EXTEND | VNOTE_LINK, 0);
        // SAFETY: one change and room for the one receipt.
        let n = unsafe {
            kevent(
                queue,
                &raw const receipt,
                1,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 1);
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        assert_eq!(got[0].data as i32, crate::errno::from_host(libc::EINVAL));
        // SAFETY: this test's own two descriptors.
        unsafe { libc::close(fd) };
        unsafe { libc::close(queue) };
    }

    /// A registration that asks for `NOTE_EXTEND` *and* something reportable is
    /// **accepted, and narrowed** - and this is the test that separates the two
    /// outcomes above: the same `NOTE_EXTEND` bit that is fatal on its own is
    /// dropped rather than fatal alongside `NOTE_WRITE`.
    ///
    /// This is the "a change list is a filter, not a demand" question answered in
    /// code: `NOTE_EXTEND` is *narrowing* of `NOTE_WRITE`, so the wider note
    /// carries the same fact with XNU's own scope, and refusing the registration
    /// outright would refuse a watch the guest can use.
    #[test]
    fn an_unreportable_note_alongside_a_reportable_one_is_dropped_not_fatal() {
        let dir = scratch("kq-vnode-narrowed");
        let file = dir.join("watched");
        std::fs::write(&file, b"before").unwrap();
        let fd = open_evtonly(&file);
        let queue = kqueue();
        // `NOTE_EXTEND | NOTE_WRITE`: the two notes a caller might use to mean
        // "the file got bigger".
        register_vnode(queue, fd, VNOTE_EXTEND | VNOTE_WRITE, 0x0A0A);

        {
            use std::io::Write;
            let mut handle = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
            handle.write_all(b"more").unwrap();
        }
        let got = collect(queue, 4);
        let notes = notes_of(&got, 4);
        assert_eq!(
            notes, VNOTE_WRITE,
            "the append is reported as NOTE_WRITE alone: NOT NOTE_EXTEND, which would claim the \
             size grew when the event that arrived does not say so"
        );

        // SAFETY: this test's own two descriptors.
        unsafe { libc::close(fd) };
        unsafe { libc::close(queue) };
    }

    /// A `NOTE_RENAME` in a registration's own `fflags` is what the framework
    /// reads back, and the guest's two `OnKQueueReadable` bodies test exactly
    /// three immediates against a delivered `fflags`: `and edx,0x8`,
    /// `test al,0x61` and `test al,0x2` (`Chromium Framework+0x5d9f1dd`,
    /// `+0x5d9f2ba` and `+0x5d9f337`, and the `__FILE__`-bracketed twin at
    /// `+0x3a9b3ad`, `+0x3a9b48a` and `+0x3a9b507`). So the five notes this
    /// implementation reports are exactly the ones the guest acts on, and the two
    /// it refuses are two the guest never tests.
    #[test]
    fn the_notes_reported_are_exactly_the_ones_the_guest_reads_back() {
        // What the two `OnKQueueReadable` bodies test against a delivered
        // `fflags`: `and edx,0x8`, `test al,0x61`, `test al,0x2`.
        const GUEST_READS: u32 = VNOTE_ATTRIB | VNOTE_WRITE | 0x61;
        // What this host can report on a measured `inotify` event.
        const REPORTED: u32 =
            VNOTE_WRITE | VNOTE_ATTRIB | VNOTE_DELETE | VNOTE_RENAME | VNOTE_REVOKE;
        // `0x61` is `NOTE_DELETE | NOTE_RENAME | NOTE_REVOKE`, so the guest reads
        // every note the implementation reports...
        assert_eq!(GUEST_READS, REPORTED);
        // ...and nothing it reports escapes what the guest reads.
        assert_eq!(REPORTED & !GUEST_READS, 0);
        // What the guest asked for and does *not* read is exactly the refused
        // pair, so narrowing `0x6f` costs this guest nothing.
        assert_eq!(ALL_SEVEN & !GUEST_READS, VNOTE_EXTEND | VNOTE_LINK);
    }

    /// A receipt for a `NOTE_RENAME` change that was **never made** is `ENOENT`,
    /// not a success.
    ///
    /// Without this, a change carrying only `EV_RECEIPT` would be read as a
    /// request to watch a path, and the caller would get a *success* receipt for a
    /// watch that was never made - which is how `EV_RECEIPT` becomes a lie.
    #[test]
    fn a_receipt_for_a_vnode_registration_that_was_never_made_is_enoent() {
        let queue = kqueue();
        let receipt = Kevent {
            filter: EVFILT_VNODE,
            flags: EV_RECEIPT,
            ..Kevent::default()
        };
        let mut got = [Kevent::default(); 1];
        // SAFETY: one change and room for the one receipt.
        let n = unsafe {
            kevent(
                queue,
                &raw const receipt,
                1,
                got.as_mut_ptr(),
                1,
                &raw const NO_WAIT,
            )
        };
        assert_eq!(n, 1, "a receipt answers with an event, not a return value");
        assert_eq!(got[0].flags & EV_ERROR, EV_ERROR);
        assert_eq!(
            got[0].data as i32,
            crate::errno::from_host(libc::ENOENT),
            "a success receipt here would be a subscription the caller does not have"
        );
        // SAFETY: this test's own queue descriptor.
        unsafe { libc::close(queue) };
    }

    /// A vnode registration's `ident` is an **open descriptor**, and the delivered
    /// event is about that descriptor - not about the queue's `inotify`, which is
    /// a different object and the only thing that ever becomes readable.
    #[test]
    fn a_vnode_ident_is_an_open_descriptor_and_the_event_is_about_it() {
        let dir = scratch("kq-vnode-ident");
        let file = dir.join("watched");
        std::fs::write(&file, b"x").unwrap();
        let fd = open_evtonly(&file);
        let queue = kqueue();
        // The premise, asserted rather than assumed: this is an `O_PATH`
        // descriptor, so `read` is `EBADF` and it could never be waited on.
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0, "it is open");
        assert_eq!(unsafe { libc::read(fd, std::ptr::null_mut(), 1) }, -1);
        assert_eq!(crate::errno::get(), libc::EBADF);
        register_vnode(queue, fd, ALL_SEVEN, 0xF00D);

        let watched = {
            let state = queue_of(queue).expect("this test's own queue");
            let state = state.lock().unwrap_or_else(|e| e.into_inner());
            let registration = &state.registrations[&(fd as usize, EVFILT_VNODE)];
            // The thing `poll` waits on is the queue's `inotify`, which is not
            // the ident - which is why `EPOLLIN` on the `O_PATH` descriptor
            // would have been `EPERM`.
            let inotify_fd = registration.vnode.as_ref().unwrap().inotify_fd();
            assert_ne!(inotify_fd, fd, "the descriptor waited on is not the ident");
            // And the narrowed interest is what the knote holds, not `0x6f`.
            assert_eq!(registration.fflags, ALL_SEVEN & !(VNOTE_EXTEND | VNOTE_LINK));
            inotify_fd
        };
        // And the `inotify` is genuinely armed on the caller's descriptor: a
        // write is reported, and the `ident` in the event is the caller's.
        {
            use std::io::Write;
            let mut handle = std::fs::OpenOptions::new().append(true).open(&file).unwrap();
            handle.write_all(b"y").unwrap();
        }
        let got = collect(queue, 4);
        assert!(notes_of(&got, 4) & VNOTE_WRITE != 0);
        assert_eq!(got[0].ident, fd as usize);
        assert_ne!(
            got[0].ident,
            watched as usize,
            "the event is about the ident, not the inotify"
        );
        // SAFETY: this test's own two descriptors.
        unsafe { libc::close(fd) };
        unsafe { libc::close(queue) };
    }
}
