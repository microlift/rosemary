//! `CFMachPort`: a Mach port this runtime can hand to a run loop.
//!
//! The port itself is [`darwin::mach`]'s, and this module is the seam between
//! that namespace and the run loop in [`crate::runloop`]. Nothing here
//! re-implements Mach: a `CFMachPort` is a name, [`darwin::mach::space`] says
//! what that name refers to, and the only thing CoreFoundation adds is a way for
//! a loop to find out that the port has something waiting.
//!
//! # Ownership
//!
//! The same model as [`crate::socket`], and for the same reason. A
//! `CFMachPortRef` here is a leaked `Arc` with no `isa` to read, registered as
//! an opaque CoreFoundation handle so that `CFRetain`, `CFRelease` and
//! `CFGetTypeID` recognise it - otherwise retaining one calls Apple's
//! `objc_retain` on a structure this crate laid out, and `CFGetTypeID` reads
//! `name` as a class pointer.
//!
//! The consequence is the one [`crate::socket`] already has: a port that is
//! never invalidated keeps its Mach right for the life of the process, and
//! releasing the object does not give the right back. `CFMachPortInvalidate` is
//! the call that does, and it is the one a guest makes when it is done with the
//! port.

#![cfg(not(target_os = "macos"))]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use crate::base::{CFIndex, CFTypeID, CFTypeRef};
use crate::runloop::Source;

use darwin::mach::space::{self, Name};

struct MachPort {
    /// The `mach_port_name_t` this wraps.
    ///
    /// Looked up in the space on demand rather than held: holding it would mean
    /// holding an `Arc<Receiver>`, and then invalidating the port would leave
    /// this struct's reference keeping the receive right's descriptor open -
    /// which is the very thing invalidation is supposed to give back.
    name: Name,
    valid: AtomicBool,
    /// The source made from this port, so invalidation can stop it. Weak because
    /// the loop holds the strong one and this is only ever a way to *reach* it.
    source: Mutex<Option<Weak<Source>>>,
}

// SAFETY: every field is an atomic or behind a lock, and `name` is a plain
// number this crate never dereferences.
unsafe impl Send for MachPort {}
unsafe impl Sync for MachPort {}

/// A `CFMachPortRef` this crate handed out, without touching its reference count.
unsafe fn borrow(port: CFTypeRef) -> Option<std::mem::ManuallyDrop<Arc<MachPort>>> {
    if port.is_null() || !crate::base::is_opaque(port) {
        return None;
    }
    // SAFETY: CFMachPort handles below are leaked `Arc<MachPort>` pointers.
    Some(std::mem::ManuallyDrop::new(unsafe {
        Arc::from_raw(port.cast::<MachPort>())
    }))
}

/// Every port this module made, weakly.
///
/// Weak so the registry cannot keep a port alive. A port is leaked, so the
/// entries are only ever dropped by [`invalidate`] taking the last word.
static PORTS: Mutex<Vec<Weak<MachPort>>> = Mutex::new(Vec::new());

/// The `CFTypeID` a `CFMachPort` reports.
///
/// The address of a byte that is in nothing in particular, which is the answer
/// `IOHIDManagerGetTypeID` and its neighbours give in this tree too: a port is a
/// handle rather than an object, so there is no class for `CFGetTypeID` to read,
/// and of the two things that could stand in for one this is the one that cannot
/// collide with a real class pointer.
static TYPE_ID: u8 = 0;

pub fn type_id() -> CFTypeID {
    std::ptr::addr_of!(TYPE_ID) as CFTypeID
}

/// `CFTypeID CFMachPortGetTypeID(void)`
pub unsafe extern "C" fn get_type_id() -> CFTypeID {
    type_id()
}

/// `CFMachPortRef CFMachPortCreateWithPort(CFAllocatorRef, mach_port_t,
/// Boolean shouldFreeInfo)`
///
/// `shouldFreeInfo` is the caller's answer to "does deallocating this object
/// give the Mach right back?", and it is deliberately not consulted: see the
/// module note on ownership. A caller that wants the right back asks for it by
/// name with [`invalidate`], which is where Apple's own documentation points for
/// exactly that.
///
/// A name nothing in the space holds is refused. Wrapping a number the host has
/// never heard of would produce a port that answers `CFMachPortIsValid` for as
/// long as nobody invalidates it and can never have anything delivered, which is
/// the failure a caller cannot see.
pub unsafe extern "C" fn create_with_port(
    _allocator: CFTypeRef,
    port: Name,
    _should_free_info: u8,
) -> CFTypeRef {
    if port == space::MACH_PORT_NULL || space::with_right(port, |_| ()).is_none() {
        return std::ptr::null();
    }
    let port = Arc::new(MachPort {
        name: port,
        valid: AtomicBool::new(true),
        source: Mutex::new(None),
    });
    PORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Arc::downgrade(&port));
    let handle = Arc::into_raw(port).cast::<c_void>();
    // A guest may `CFRetain` this and `CFGetTypeID` it, and neither has an `isa`
    // to go through.
    crate::base::note_typed_opaque(handle, type_id());
    handle
}

/// `CFRunLoopSourceRef CFMachPortCreateRunLoopSource(CFAllocatorRef, CFMachPortRef,
/// CFIndex order)`
///
/// The `order` is Apple's own sort key between the version-0 sources a loop
/// holds, and it is not consulted for the same reason it is not in
/// [`crate::socket::create_run_loop_source`]: this implementation has one kind of
/// source and the loop runs them in the order it was given them.
///
/// Null rather than a source that never fires when the port cannot be listened
/// to. A name that is not a receive right - a send right, a port set - has no
/// queue to drain, and a source that never fires is a program that hangs with
/// nothing anywhere to say why.
pub unsafe extern "C" fn create_run_loop_source(
    _allocator: CFTypeRef,
    port: CFTypeRef,
    _order: CFIndex,
) -> CFTypeRef {
    let Some(port) = (unsafe { borrow(port) }) else {
        return std::ptr::null();
    };
    if !port.valid.load(Ordering::Acquire) || space::receiver(port.name).is_none() {
        return std::ptr::null();
    }
    let source =
        crate::runloop::new_source_ready(Some(perform), Arc::as_ptr(&port) as usize, Some(ready));
    *port.source.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(&source));
    // `runloop::new_source_ready` has already recorded the handle, so the loop's
    // own `CFRunLoopSource*` entry points can find it again.
    Arc::into_raw(source).cast::<c_void>()
}

/// Whether the `CFMachPort` at `info` has a message waiting.
///
/// Handed to the source as its readiness predicate and given the source's own
/// `info` - the port handle, which is what [`perform`] is given too - so one
/// function serves every port and a `CFMachPort` costs no closure. See
/// [`crate::runloop::Source::ready`].
fn ready(info: usize) -> bool {
    // SAFETY: a handle `create_with_port` handed out, which is what `info` is: the
    // source was built from one and nothing else can put a port here.
    let Some(port) = (unsafe { borrow(info as CFTypeRef) }) else {
        return false;
    };
    space::receiver(port.name).is_some_and(|receiver| receiver.ready())
}

/// Drain whatever has arrived, when the loop has said the port is readable.
unsafe extern "C" fn perform(info: *mut c_void) {
    let Some(port) = (unsafe { borrow(info.cast()) }) else {
        return;
    };
    if !port.valid.load(Ordering::Acquire) {
        return;
    }
    // The queue is emptied and the messages are dropped, which is what Mach does
    // with a message a receiver discards - including any rights inside it, which
    // `Datagram`'s destructor destroys.
    //
    // Nothing in this tree has asked for a `CFMachPortCallout`, so there is
    // nowhere to put a message that would mean anything. The alternative -
    // leaving them queued - is not "delivery deferred", it is a port that is
    // permanently readable and a loop that spins on it forever.
    while let Some(receiver) = space::receiver(port.name)
        && matches!(receiver.receive(0), Ok(Some(_)))
    {}
}

/// `CFMachPortInvalidate(CFMachPortRef)`
///
/// The Mach right is destroyed, which is what stops delivery: the name is gone
/// from the space, so nothing can be sent to it any more and a loop that is
/// turning finds no port to read. The source made from the port is invalidated
/// too, so a loop that already holds one stops being woken by it.
///
/// Idempotent, because a second invalidation of an already-invalid port has
/// nothing left to destroy and the name is gone either way.
pub unsafe extern "C" fn invalidate(port: CFTypeRef) {
    let Some(port) = (unsafe { borrow(port) }) else {
        return;
    };
    if !port.valid.swap(false, Ordering::AcqRel) {
        return;
    }
    space::destroy(port.name);
    if let Some(source) = port
        .source
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(Weak::upgrade)
    {
        source.invalidate();
    }
}

/// `Boolean CFMachPortIsValid(CFMachPortRef)`
pub unsafe extern "C" fn is_valid(port: CFTypeRef) -> u8 {
    unsafe { borrow(port) }.is_some_and(|port| port.valid.load(Ordering::Acquire)) as u8
}

/// `mach_port_t CFMachPortGetPort(CFMachPortRef)`
pub unsafe extern "C" fn get_port(port: CFTypeRef) -> Name {
    unsafe { borrow(port) }.map_or(space::MACH_PORT_NULL, |port| port.name)
}

/// Signal the source of every live port that has a message waiting.
///
/// The run loop asks this once a turn, for the reason [`crate::socket::poll`]
/// does: the loop's sleep is a condition variable, so something has to look at
/// the descriptors and the ports before it decides how long it may sleep. A run
/// loop source cannot read a port - only the port can - so this is where the two
/// meet.
///
/// The sweep is **process-wide** rather than per-loop, because nothing here knows
/// which loop a port's source was added to and every loop's turn is a chance to
/// notice a message. The cost is that a turn on one thread can signal a source
/// belonging to another's loop, and the source then finds the port already
/// drained - which is what [`ready`] is for, so that the turn still reports what
/// it delivered.
pub(crate) fn poll() {
    let ports: Vec<Arc<MachPort>> = PORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    for port in ports {
        if !port.valid.load(Ordering::Acquire) {
            continue;
        }
        if !space::receiver(port.name).is_some_and(|receiver| receiver.ready()) {
            continue;
        }
        if let Some(source) = port
            .source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(Weak::upgrade)
        {
            source.signal();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A receive right out of the port space.
    fn receive_right() -> Name {
        let mut name = space::MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        assert_eq!(
            unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_RECEIVE, &mut name) },
            0,
            "KERN_SUCCESS"
        );
        name
    }

    /// A loop of this crate's own, so a test does not disturb the thread's.
    fn loop_() -> Arc<crate::runloop::RunLoop> {
        Arc::new(crate::runloop::RunLoop::default())
    }

    /// A message sent to `name`, from outside the process's own loop.
    fn send(name: Name) {
        let address = space::address_of(name).expect("a receive right has an address");
        // SAFETY: an address this process bound.
        darwin::mach::wire::send(address, b"ping", &[]).expect("the port is listening");
    }

    /// A source made from `port` and added to a loop of this test's, which is the
    /// whole shape a guest builds.
    fn bridged(port: CFTypeRef) -> (Arc<crate::runloop::RunLoop>, Arc<Source>) {
        let loop_ = loop_();
        // SAFETY: a port this module made.
        let handle = unsafe { create_run_loop_source(std::ptr::null(), port, 0) };
        assert!(!handle.is_null(), "a receive right can be listened to");
        // SAFETY: `create_run_loop_source` returned a leaked `Arc<Source>`, and it
        // is not dropped: the loop holds the only reference that matters.
        let source = unsafe { Arc::from_raw(handle.cast::<Source>()) };
        loop_.add_source(&source, crate::runloop::DEFAULT_MODE);
        (loop_, source)
    }

    /// What a guest observes, in the order it happens: a port that is quiet, one
    /// with a message on it, and one that has been invalidated.
    ///
    /// Ungated, like `darwin::mach::space`'s own port tests: allocating a receive
    /// right *is* the thing being tested, and a test that could be turned off is a
    /// test that says nothing on the day it is turned off.
    #[test]
    fn a_message_wakes_the_source_and_invalidation_stops_it() {
        let name = receive_right();
        // SAFETY: a null allocator and a name this test allocated.
        let port = unsafe { create_with_port(std::ptr::null(), name, 1) };
        assert!(!port.is_null());
        // SAFETY: an object this test made.
        unsafe {
            assert_eq!(get_type_id(), crate::base::opaque_type_id(port).unwrap());
            assert_eq!(is_valid(port), 1);
            assert_eq!(get_port(port), name);
        }

        let (loop_, source) = bridged(port);
        // Nothing sent, so there is nothing to deliver.
        assert_eq!(
            loop_.run_in_mode(crate::runloop::DEFAULT_MODE, 0.0, true),
            crate::runloop::result::TIMED_OUT
        );

        send(name);
        assert_eq!(
            loop_.run_in_mode(crate::runloop::DEFAULT_MODE, 1.0, true),
            crate::runloop::result::HANDLED_SOURCE,
            "a message waiting is a source the loop services"
        );
        // Drained, so the next turn is a quiet one: `perform` empties the queue.
        assert_eq!(
            loop_.run_in_mode(crate::runloop::DEFAULT_MODE, 0.0, true),
            crate::runloop::result::TIMED_OUT,
            "the source fired again: the queue was not emptied"
        );

        // SAFETY: the port and source this test made.
        unsafe {
            invalidate(port);
            assert_eq!(is_valid(port), 0, "invalidation is final");
            assert_eq!(
                crate::api::CFRunLoopSourceIsValid(Arc::as_ptr(&source) as CFTypeRef),
                0,
                "and it stops the source the port made"
            );
        }
        // The Mach right is gone, so nothing can be sent to it any more - which is
        // what "stops delivery" means.
        assert!(
            space::with_right(name, |_| ()).is_none(),
            "the name is no longer in the space"
        );
        // Nothing valid is left, so the mode has nothing to service at all.
        assert_eq!(
            loop_.run_in_mode(crate::runloop::DEFAULT_MODE, 0.0, true),
            crate::runloop::result::FINISHED
        );
    }

    /// A second invalidation is a no-op: the name is gone either way, and
    /// destroying it again must not be an error a caller has to see.
    #[test]
    fn invalidating_twice_is_the_same_as_once() {
        let name = receive_right();
        // SAFETY: a null allocator and a name this test allocated.
        let port = unsafe { create_with_port(std::ptr::null(), name, 1) };
        let (loop_, _source) = bridged(port);
        send(name);
        assert_eq!(
            loop_.run_in_mode(crate::runloop::DEFAULT_MODE, 1.0, true),
            crate::runloop::result::HANDLED_SOURCE
        );
        // SAFETY: the port this test made.
        unsafe {
            invalidate(port);
            invalidate(port);
            assert_eq!(is_valid(port), 0);
        }
    }

    /// A name that is not a receive right cannot be listened to, a name nothing
    /// holds is not a port at all, and an invalidated port has no source.
    #[test]
    fn only_a_live_receive_right_becomes_a_source() {
        let mut set = space::MACH_PORT_NULL;
        // SAFETY: a live local for the out-parameter.
        assert_eq!(
            unsafe { space::mach_port_allocate(space::TASK_SELF, space::RIGHT_PORT_SET, &mut set) },
            0
        );
        // SAFETY: a port set wrapped as a port, and a name nothing in the space
        // holds.
        unsafe {
            let not_a_receive_right = create_with_port(std::ptr::null(), set, 1);
            assert!(
                !not_a_receive_right.is_null(),
                "the object exists; there is no queue behind it"
            );
            assert!(
                create_run_loop_source(std::ptr::null(), not_a_receive_right, 0).is_null(),
                "a port set has no queue to drain"
            );
            assert!(
                create_with_port(std::ptr::null(), 0x7fff_0000, 1).is_null(),
                "a name the host has never heard of is not a port"
            );
            assert!(
                create_with_port(std::ptr::null(), space::MACH_PORT_NULL, 1).is_null(),
                "and MACH_PORT_NULL is not one either"
            );

            let live = receive_right();
            let port = create_with_port(std::ptr::null(), live, 1);
            invalidate(port);
            assert!(
                create_run_loop_source(std::ptr::null(), port, 0).is_null(),
                "an invalid port has no source"
            );
            // And nothing that is not a port at all is read as one.
            assert!(create_run_loop_source(std::ptr::null(), std::ptr::null(), 0).is_null());
            assert_eq!(is_valid(std::ptr::null()), 0);
            assert_eq!(get_port(std::ptr::null()), space::MACH_PORT_NULL);
        }
        // The port set was never this test's to keep.
        space::mach_port_destroy(space::TASK_SELF, set);
    }

    /// A message that arrived before the source existed is still delivered: the
    /// loop asks the port, not the order things happened to be set up in.
    #[test]
    fn a_message_that_arrived_first_is_still_delivered() {
        let name = receive_right();
        send(name);
        // SAFETY: a null allocator and a name this test allocated.
        let port = unsafe { create_with_port(std::ptr::null(), name, 1) };
        let (loop_, _source) = bridged(port);
        assert_eq!(
            loop_.run_in_mode(crate::runloop::DEFAULT_MODE, 1.0, true),
            crate::runloop::result::HANDLED_SOURCE
        );
    }
}
