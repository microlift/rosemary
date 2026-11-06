//! A minimal `CFReadStream`/`CFWriteStream` pair - just enough for
//! `CFStreamCreateBoundPair` to hand back two correctly paired, correctly
//! reference-counted handles. Real stream operations (`CFReadStreamOpen`,
//! `CFReadStreamRead`, scheduling in a run loop, client callbacks...) are not
//! implemented; nothing in this tree has asked for them yet, and a program
//! that only means to hand one end to a library that reads or writes it still
//! gets a real, usable object at this layer.
//!
//! Like [`crate::runloop`]'s sources and timers, a stream here is an opaque
//! handle with no Objective-C `isa` - a leaked `Arc`, noted with
//! [`crate::base::note_opaque`] so the ordinary `CFRetain`/`CFRelease`/
//! `CFGetTypeID` path recognizes it.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::base::CFTypeRef;

/// The shared, in-memory pipe a bound pair moves bytes through.
///
/// Read by nothing yet outside `#[cfg(test)]`: no `CFReadStreamRead` or
/// `CFWriteStreamWrite` exists here for the bytes to move through, only the
/// pairing this module's own `Create` answers for.
#[allow(dead_code)]
pub struct Pipe {
    pub buffer: Mutex<VecDeque<u8>>,
}

/// One end of a bound pair. Which one is [`Stream::is_read`]. As [`Pipe`], its
/// fields are read today only by this module's own test.
#[allow(dead_code)]
pub struct Stream {
    pub pipe: Arc<Pipe>,
    pub is_read: bool,
}

/// Build a connected read/write pair over one shared buffer, as
/// `CFStreamCreateBoundPair(CFAllocatorRef, CFReadStreamRef *, CFWriteStreamRef
/// *, CFIndex bufferSize)` does.
///
/// `buffer_size` is the hint Apple's version preallocates with; this one
/// grows to hold whatever is written instead, the way `CFArrayCreateMutable`'s
/// capacity hint is ignored for the same reason.
pub fn new_bound_pair() -> (CFTypeRef, CFTypeRef) {
    let pipe = Arc::new(Pipe {
        buffer: Mutex::new(VecDeque::new()),
    });
    let read = Arc::new(Stream {
        pipe: pipe.clone(),
        is_read: true,
    });
    let write = Arc::new(Stream {
        pipe,
        is_read: false,
    });
    crate::runloop::note(Arc::as_ptr(&read));
    crate::runloop::note(Arc::as_ptr(&write));
    (
        Arc::into_raw(read) as CFTypeRef,
        Arc::into_raw(write) as CFTypeRef,
    )
}

/// `void CFStreamCreateBoundPair(CFAllocatorRef, CFReadStreamRef *readStream,
/// CFWriteStreamRef *writeStream, CFIndex bufferSize)`.
///
/// # Safety
///
/// `read_stream` and `write_stream` are each null or writable.
#[allow(non_snake_case)]
pub unsafe extern "C" fn CFStreamCreateBoundPair(
    _allocator: CFTypeRef,
    read_stream: *mut CFTypeRef,
    write_stream: *mut CFTypeRef,
    _buffer_size: crate::base::CFIndex,
) {
    let (read, write) = new_bound_pair();
    if !read_stream.is_null() {
        // SAFETY: writable, per the caller.
        unsafe { read_stream.write(read) };
    }
    if !write_stream.is_null() {
        // SAFETY: as above.
        unsafe { write_stream.write(write) };
    }
}

exports::exports! {
    functions {
        "CFStreamCreateBoundPair" => CFStreamCreateBoundPair,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bound_pair_is_one_read_end_and_one_write_end_sharing_a_pipe() {
        let (read, write) = new_bound_pair();
        // SAFETY: freshly built handles, borrowed rather than consumed - this
        // test does not release them, which is the same choice `runloop`'s own
        // tests make over a leaked handle.
        unsafe {
            let read = &*(read as *const Stream);
            let write = &*(write as *const Stream);
            assert!(read.is_read);
            assert!(!write.is_read);
            assert!(Arc::ptr_eq(&read.pipe, &write.pipe));
        }
    }
}
