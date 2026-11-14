//! `CFReadStream` over a file or a block of bytes, toll-free bridged with
//! `NSInputStream` as on macOS: one object is both, so a stream made with
//! either API can be handed to the other.
//!
//! Only the synchronous half is here. A stream that is never scheduled on a run
//! loop reads synchronously on macOS too, which is how property-list and JSON
//! readers use it; `CFReadStreamSetClient` and run-loop scheduling are not
//! provided, so a caller that needs them is refused by name.

// The C spellings are the interface; renaming them would only hide it.
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::io::Read;

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use crate::api::{Boolean, CFAllocatorRef};
use crate::base::{CFIndex, CFTypeID, CFTypeRef};

/// `CFStreamStatus`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(isize)]
pub enum Status {
    NotOpen = 0,
    Open = 2,
    AtEnd = 5,
    Closed = 6,
    Error = 7,
}

enum Source {
    File(std::path::PathBuf),
    Bytes(Vec<u8>),
}

struct State {
    source: Source,
    status: Status,
    file: Option<std::fs::File>,
    /// How far into `Source::Bytes` reading has got.
    offset: usize,
    errno: i32,
}

#[repr(C)]
struct Body {
    isa: *const Class,
    state: *mut State,
}

/// `__NSCFInputStream`, the concrete class under `NSInputStream`.
pub fn class() -> &'static Class {
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let ptr = *CLASS.get_or_init(|| {
        let stream = crate::base::abstract_class(c"NSStream", None);
        let input = crate::base::abstract_class(c"NSInputStream", Some(stream));
        crate::base::note_type_class(input);
        let class =
            crate::base::register(c"__NSCFInputStream", Some(input), size_of::<Body>(), false);
        install_methods(input, class);
        objc::class::ptr(class) as usize
    });
    // SAFETY: the cell holds a pointer to a class this function registered.
    unsafe { objc::class::from_ptr(ptr as *const Class).expect("a class") }
}

/// A new stream at +1 over `source`, not yet open.
fn new(source: Source) -> CFTypeRef {
    // SAFETY: `Body` begins with `isa` and is the layout `class` was registered
    // with.
    let body: *mut Body = unsafe { crate::base::allocate(class()) };
    if body.is_null() {
        return std::ptr::null();
    }
    // SAFETY: a live instance whose state word is ours to fill in.
    unsafe { set_source(body, source) };
    body as CFTypeRef
}

/// # Safety
///
/// `body` is a live `__NSCFInputStream` with no state yet.
unsafe fn set_source(body: *mut Body, source: Source) {
    let state = Box::new(State {
        source,
        status: Status::NotOpen,
        file: None,
        offset: 0,
        errno: 0,
    });
    // SAFETY: the caller's.
    unsafe { (*body).state = Box::into_raw(state) };
}

/// The state of `stream`, if it is an initialized stream of this runtime.
///
/// # Safety
///
/// `stream` must be null or an object this runtime allocated, and no other
/// reference to its state may be live.
unsafe fn state<'a>(stream: CFTypeRef) -> Option<&'a mut State> {
    if !unsafe { crate::base::is_kind_of(stream, class()) } {
        return None;
    }
    // SAFETY: an instance of `class()` is a `Body`.
    unsafe { (*(stream as *mut Body)).state.as_mut() }
}

impl State {
    fn open(&mut self) -> bool {
        if self.status != Status::NotOpen {
            return false;
        }
        match &self.source {
            Source::File(path) => match std::fs::File::open(path) {
                Ok(file) => {
                    self.file = Some(file);
                    self.status = Status::Open;
                }
                Err(e) => {
                    self.errno = e.raw_os_error().unwrap_or(libc::EIO);
                    self.status = Status::Error;
                }
            },
            Source::Bytes(_) => self.status = Status::Open,
        }
        self.status == Status::Open
    }

    /// `CFReadStreamRead`'s answer: bytes read, 0 at the end, -1 on error or
    /// when the stream is not open.
    fn read(&mut self, buffer: &mut [u8]) -> CFIndex {
        match self.status {
            Status::Open => {}
            Status::AtEnd => return 0,
            _ => return -1,
        }
        let read = match &self.source {
            Source::File(_) => {
                let file = self.file.as_mut().expect("an open file stream has a file");
                loop {
                    match file.read(buffer) {
                        Ok(n) => break n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            self.errno = e.raw_os_error().unwrap_or(libc::EIO);
                            self.status = Status::Error;
                            return -1;
                        }
                    }
                }
            }
            Source::Bytes(bytes) => {
                let n = buffer.len().min(bytes.len() - self.offset);
                buffer[..n].copy_from_slice(&bytes[self.offset..self.offset + n]);
                self.offset += n;
                n
            }
        };
        if read == 0 && !buffer.is_empty() {
            self.status = Status::AtEnd;
        }
        read as CFIndex
    }

    fn close(&mut self) {
        self.file = None;
        if self.status != Status::NotOpen {
            self.status = Status::Closed;
        }
    }
}

/// Everything left in `stream`, read synchronously, or `None` on an error or a
/// stream that is not open. `limit` stops after that many bytes when nonzero.
///
/// # Safety
///
/// `stream` must be null or an object this runtime allocated.
pub unsafe fn read_to_end(stream: CFTypeRef, limit: usize) -> Option<Vec<u8>> {
    // SAFETY: the caller's.
    let state = unsafe { state(stream) }?;
    let mut out = Vec::new();
    let mut chunk = [0u8; 1 << 16];
    loop {
        let want = match limit {
            0 => chunk.len(),
            _ => chunk.len().min(limit - out.len()),
        };
        if want == 0 {
            return Some(out);
        }
        match state.read(&mut chunk[..want]) {
            0 => return Some(out),
            n if n > 0 => out.extend_from_slice(&chunk[..n as usize]),
            _ => return None,
        }
    }
}

/// The stream's error as a new `NSError` in `NSPOSIXErrorDomain`, or null.
fn copy_error(state: &State) -> CFTypeRef {
    if state.status != Status::Error {
        return std::ptr::null();
    }
    let Some(class) = objc::class::lookup(c"NSError") else {
        return std::ptr::null();
    };
    let domain = crate::string::from_bytes(b"NSPOSIXErrorDomain", false);
    let sel = objc::sel::get("errorWithDomain:code:userInfo:");
    // SAFETY: `+errorWithDomain:code:userInfo:` is `id (Class, SEL, id,
    // NSInteger, id)`, installed by Foundation.
    let object = unsafe {
        let f: unsafe extern "C" fn(Id, Sel, CFTypeRef, i64, CFTypeRef) -> Id =
            std::mem::transmute(objc::msg::entry());
        f(
            objc::class::ptr(class) as Id,
            sel,
            domain,
            i64::from(state.errno),
            std::ptr::null(),
        )
    };
    let object = objc::arc::retain(object) as CFTypeRef;
    objc::arc::release(domain as Id);
    object
}

/// `CFTypeID CFReadStreamGetTypeID(void)`
pub unsafe extern "C" fn CFReadStreamGetTypeID() -> CFTypeID {
    crate::base::type_id_of(class())
}

/// `CFReadStreamRef CFReadStreamCreateWithFile(CFAllocatorRef, CFURLRef)`
///
/// Null unless `url` is a `file:` URL. The file is opened by `CFReadStreamOpen`,
/// so a missing one is that call's error, as on macOS.
///
/// # Safety
///
/// `url` is null or an object this runtime allocated.
pub unsafe extern "C" fn CFReadStreamCreateWithFile(
    _allocator: CFAllocatorRef,
    url: CFTypeRef,
) -> CFTypeRef {
    // SAFETY: the caller's.
    let Some(path) = (unsafe { crate::url::as_url(url) }).and_then(|u| u.file_path()) else {
        return std::ptr::null();
    };
    new(Source::File(prefix::ns::host_path(&path)))
}

/// `CFReadStreamRef CFReadStreamCreateWithBytesNoCopy(CFAllocatorRef,
/// const UInt8 *, CFIndex, CFAllocatorRef bytesDeallocator)`
///
/// The bytes are copied, and `bytesDeallocator` is applied at once: they are
/// read from the copy for the stream's whole life, which is the observable
/// behaviour, and the caller gave them up when it named a deallocator.
///
/// # Safety
///
/// `bytes` is readable for `length` bytes.
pub unsafe extern "C" fn CFReadStreamCreateWithBytesNoCopy(
    _allocator: CFAllocatorRef,
    bytes: *const u8,
    length: CFIndex,
    deallocator: CFAllocatorRef,
) -> CFTypeRef {
    let Ok(length) = usize::try_from(length) else {
        return std::ptr::null();
    };
    let copy = match length {
        0 => Vec::new(),
        // SAFETY: the caller's.
        _ => unsafe { std::slice::from_raw_parts(bytes, length) }.to_vec(),
    };
    if !bytes.is_null() {
        // SAFETY: the deallocator owns `bytes`, which nothing here keeps.
        unsafe { crate::api::give_back(deallocator, bytes.cast_mut().cast()) };
    }
    new(Source::Bytes(copy))
}

/// `Boolean CFReadStreamOpen(CFReadStreamRef)`
///
/// # Safety
///
/// `stream` is null or an object this runtime allocated.
pub unsafe extern "C" fn CFReadStreamOpen(stream: CFTypeRef) -> Boolean {
    // SAFETY: the caller's.
    unsafe { state(stream) }.is_some_and(State::open) as Boolean
}

/// `void CFReadStreamClose(CFReadStreamRef)`
///
/// # Safety
///
/// `stream` is null or an object this runtime allocated.
pub unsafe extern "C" fn CFReadStreamClose(stream: CFTypeRef) {
    // SAFETY: the caller's.
    if let Some(state) = unsafe { state(stream) } {
        state.close();
    }
}

/// `CFIndex CFReadStreamRead(CFReadStreamRef, UInt8 *buffer, CFIndex length)`
///
/// # Safety
///
/// `stream` is null or an object this runtime allocated; `buffer` is writable
/// for `length` bytes.
pub unsafe extern "C" fn CFReadStreamRead(
    stream: CFTypeRef,
    buffer: *mut u8,
    length: CFIndex,
) -> CFIndex {
    // SAFETY: the caller's.
    let Some(state) = (unsafe { state(stream) }) else {
        return -1;
    };
    let Ok(length) = usize::try_from(length) else {
        return -1;
    };
    if length == 0 {
        return 0;
    }
    // SAFETY: the caller's.
    state.read(unsafe { std::slice::from_raw_parts_mut(buffer, length) })
}

/// `CFStreamStatus CFReadStreamGetStatus(CFReadStreamRef)`
///
/// # Safety
///
/// `stream` is null or an object this runtime allocated.
pub unsafe extern "C" fn CFReadStreamGetStatus(stream: CFTypeRef) -> CFIndex {
    // SAFETY: the caller's.
    unsafe { state(stream) }.map_or(Status::Error, |s| s.status) as CFIndex
}

/// `Boolean CFReadStreamHasBytesAvailable(CFReadStreamRef)`
///
/// An open file or byte stream can always be read without blocking, as on
/// macOS: the next read says whether anything was left.
///
/// # Safety
///
/// `stream` is null or an object this runtime allocated.
pub unsafe extern "C" fn CFReadStreamHasBytesAvailable(stream: CFTypeRef) -> Boolean {
    // SAFETY: the caller's.
    unsafe { state(stream) }.is_some_and(|s| s.status == Status::Open) as Boolean
}

/// `CFErrorRef CFReadStreamCopyError(CFReadStreamRef)`
///
/// # Safety
///
/// `stream` is null or an object this runtime allocated.
pub unsafe extern "C" fn CFReadStreamCopyError(stream: CFTypeRef) -> CFTypeRef {
    // SAFETY: the caller's.
    unsafe { state(stream) }.map_or(std::ptr::null(), |s| copy_error(s))
}

/// `CFPropertyListRef CFPropertyListCreateWithStream(CFAllocatorRef,
/// CFReadStreamRef, CFIndex streamLength, CFOptionFlags, CFPropertyListFormat *,
/// CFErrorRef *)`
///
/// Reads the open stream to its end, or `streamLength` bytes when that is
/// nonzero, and parses what it read as `CFPropertyListCreateWithData` does.
///
/// # Safety
///
/// `stream` is null or an object this runtime allocated; `format` and `error`
/// are null or writable for one value.
pub unsafe extern "C" fn CFPropertyListCreateWithStream(
    allocator: CFAllocatorRef,
    stream: CFTypeRef,
    length: CFIndex,
    options: u64,
    format: *mut CFIndex,
    error: *mut CFTypeRef,
) -> CFTypeRef {
    let limit = usize::try_from(length).unwrap_or(0);
    // SAFETY: the caller's.
    let Some(bytes) = (unsafe { read_to_end(stream, limit) }) else {
        if !error.is_null() {
            // SAFETY: writable, per the caller.
            unsafe {
                *error = state(stream).map_or(std::ptr::null(), |s| copy_error(s));
            }
        }
        return std::ptr::null();
    };
    let data = crate::collections::new_data(bytes);
    // SAFETY: `data` is a CFData of this runtime; the rest is the caller's.
    let list = unsafe {
        crate::propertylist::CFPropertyListCreateWithData(allocator, data, options, format, error)
    };
    objc::arc::release(data as Id);
    list
}

// `NSInputStream`, which is the same object.

/// `+[NSInputStream allocWithZone:]`: the class cluster's concrete class, as
/// for `alloc`; a guest's own subclass allocates itself.
unsafe extern "C" fn alloc_with_zone(receiver: Id, _sel: Sel, _zone: *mut c_void) -> Id {
    let input = objc::class::lookup(c"NSInputStream").expect("registered with the stream class");
    if receiver != objc::class::ptr(input) as Id {
        // SAFETY: `receiver` is a class: this is a class method.
        let class = unsafe { objc::class::from_ptr(receiver as *const Class) }.expect("a class");
        return objc::object::create_instance(class, 0);
    }
    objc::object::create_instance(class(), 0)
}

unsafe extern "C" fn alloc(receiver: Id, sel: Sel) -> Id {
    // SAFETY: as `allocWithZone:`, which ignores the zone.
    unsafe { alloc_with_zone(receiver, sel, std::ptr::null_mut()) }
}

/// Initialize `receiver` with `source`, or release it and answer nil if it is
/// not an uninitialized stream of this runtime.
///
/// # Safety
///
/// `receiver` is null or an object this runtime allocated, at +1.
unsafe fn init_with(receiver: Id, source: Option<Source>) -> Id {
    let fresh = unsafe { crate::base::is_kind_of(receiver as CFTypeRef, class()) }
        // SAFETY: an instance of `class()` is a `Body`.
        && unsafe { (*(receiver as *mut Body)).state.is_null() };
    match (fresh, source) {
        (true, Some(source)) => {
            // SAFETY: checked just above.
            unsafe { set_source(receiver as *mut Body, source) };
            receiver
        }
        _ => {
            objc::arc::release(receiver);
            std::ptr::null_mut()
        }
    }
}

/// The host path of an `NSString` guest path.
///
/// # Safety
///
/// `path` is null or an object this runtime allocated.
unsafe fn host_path_of(path: Id) -> Option<std::path::PathBuf> {
    // SAFETY: the caller's.
    let text = unsafe { crate::string::as_string(path as CFTypeRef) }?.utf8();
    let text = String::from_utf8(text).ok()?;
    Some(prefix::ns::host_path(&text))
}

unsafe extern "C" fn init_with_file_at_path(receiver: Id, _sel: Sel, path: Id) -> Id {
    // SAFETY: a message's receiver and argument are null or live objects.
    unsafe { init_with(receiver, host_path_of(path).map(Source::File)) }
}

unsafe extern "C" fn init_with_url(receiver: Id, _sel: Sel, url: Id) -> Id {
    // SAFETY: as above.
    let path = unsafe { crate::url::as_url(url as CFTypeRef) }.and_then(|u| u.file_path());
    // SAFETY: as above.
    unsafe {
        init_with(
            receiver,
            path.map(|p| Source::File(prefix::ns::host_path(&p))),
        )
    }
}

unsafe extern "C" fn init_with_data(receiver: Id, _sel: Sel, data: Id) -> Id {
    // SAFETY: as above.
    let bytes = unsafe { crate::collections::data(data as CFTypeRef) }.map(|b| b.clone());
    // SAFETY: as above.
    unsafe { init_with(receiver, bytes.map(Source::Bytes)) }
}

/// `+inputStreamWith...:`: `-initWith...:` on a new instance, autoreleased.
fn convenience(receiver: Id, init: unsafe extern "C" fn(Id, Sel, Id) -> Id, arg: Id) -> Id {
    // SAFETY: `receiver` is the class the message went to.
    let object = unsafe { alloc(receiver, objc::sel::get("alloc")) };
    // SAFETY: a fresh instance at +1, and the argument the caller passed.
    let object = unsafe { init(object, objc::sel::get("init"), arg) };
    objc::arc::autorelease(object)
}

unsafe extern "C" fn stream_with_file_at_path(receiver: Id, _sel: Sel, path: Id) -> Id {
    convenience(receiver, init_with_file_at_path, path)
}

unsafe extern "C" fn stream_with_url(receiver: Id, _sel: Sel, url: Id) -> Id {
    convenience(receiver, init_with_url, url)
}

unsafe extern "C" fn stream_with_data(receiver: Id, _sel: Sel, data: Id) -> Id {
    convenience(receiver, init_with_data, data)
}

unsafe extern "C" fn open(receiver: Id, _sel: Sel) {
    // SAFETY: the receiver of a message to this class.
    unsafe { CFReadStreamOpen(receiver as CFTypeRef) };
}

unsafe extern "C" fn close(receiver: Id, _sel: Sel) {
    // SAFETY: as above.
    unsafe { CFReadStreamClose(receiver as CFTypeRef) };
}

unsafe extern "C" fn read(receiver: Id, _sel: Sel, buffer: *mut u8, length: usize) -> isize {
    // SAFETY: as above; `-read:maxLength:`'s buffer contract is
    // `CFReadStreamRead`'s.
    unsafe { CFReadStreamRead(receiver as CFTypeRef, buffer, length as CFIndex) }
}

/// `-getBuffer:length:`: NO, as macOS answers for a file stream, which has no
/// internal buffer to lend.
unsafe extern "C" fn get_buffer(_: Id, _: Sel, _: *mut *mut u8, _: *mut usize) -> bool {
    false
}

unsafe extern "C" fn has_bytes_available(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: as above.
    unsafe { CFReadStreamHasBytesAvailable(receiver as CFTypeRef) != 0 }
}

unsafe extern "C" fn stream_status(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: as above.
    unsafe { CFReadStreamGetStatus(receiver as CFTypeRef) as usize }
}

unsafe extern "C" fn stream_error(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: as above.
    objc::arc::autorelease(unsafe { CFReadStreamCopyError(receiver as CFTypeRef) } as Id)
}

unsafe extern "C" fn dealloc(receiver: Id, _sel: Sel) {
    // SAFETY: `-dealloc` is reached with the last reference gone, and the
    // receiver is a `Body` whose state, if any, came from `Box::into_raw`.
    unsafe {
        let body = receiver as *mut Body;
        if !(*body).state.is_null() {
            drop(Box::from_raw((*body).state));
            (*body).state = std::ptr::null_mut();
        }
        objc::arc::finalize(receiver);
    }
}

fn install_methods(input: &'static Class, concrete: &'static Class) {
    let meta = input.metaclass();
    for (sel, imp, types) in [
        ("alloc", alloc as *const c_void, c"@16@0:8"),
        (
            "allocWithZone:",
            alloc_with_zone as *const c_void,
            c"@24@0:8^v16",
        ),
        (
            "inputStreamWithFileAtPath:",
            stream_with_file_at_path as *const c_void,
            c"@24@0:8@16",
        ),
        (
            "inputStreamWithURL:",
            stream_with_url as *const c_void,
            c"@24@0:8@16",
        ),
        (
            "inputStreamWithData:",
            stream_with_data as *const c_void,
            c"@24@0:8@16",
        ),
    ] {
        meta.replace_method(objc::sel::get(sel), imp, types);
    }
    for (sel, imp, types) in [
        (
            "initWithFileAtPath:",
            init_with_file_at_path as *const c_void,
            c"@24@0:8@16",
        ),
        (
            "initWithURL:",
            init_with_url as *const c_void,
            c"@24@0:8@16",
        ),
        (
            "initWithData:",
            init_with_data as *const c_void,
            c"@24@0:8@16",
        ),
        ("open", open as *const c_void, c"v16@0:8"),
        ("close", close as *const c_void, c"v16@0:8"),
        ("read:maxLength:", read as *const c_void, c"q32@0:8*16Q24"),
        (
            "getBuffer:length:",
            get_buffer as *const c_void,
            c"B32@0:8^*16^Q24",
        ),
        (
            "hasBytesAvailable",
            has_bytes_available as *const c_void,
            c"B16@0:8",
        ),
        ("streamStatus", stream_status as *const c_void, c"Q16@0:8"),
        ("streamError", stream_error as *const c_void, c"@16@0:8"),
        ("dealloc", dealloc as *const c_void, c"v16@0:8"),
    ] {
        concrete.replace_method(objc::sel::get(sel), imp, types);
    }
}

exports::exports! {
    functions {
        "CFReadStreamGetTypeID" => CFReadStreamGetTypeID,
        "CFReadStreamCreateWithFile" => CFReadStreamCreateWithFile,
        "CFReadStreamCreateWithBytesNoCopy" => CFReadStreamCreateWithBytesNoCopy,
        "CFReadStreamOpen" => CFReadStreamOpen,
        "CFReadStreamClose" => CFReadStreamClose,
        "CFReadStreamRead" => CFReadStreamRead,
        "CFReadStreamGetStatus" => CFReadStreamGetStatus,
        "CFReadStreamHasBytesAvailable" => CFReadStreamHasBytesAvailable,
        "CFReadStreamCopyError" => CFReadStreamCopyError,
        "CFPropertyListCreateWithStream" => CFPropertyListCreateWithStream,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(stream: CFTypeRef) -> Vec<u8> {
        // SAFETY: a stream this test made.
        unsafe { read_to_end(stream, 0) }.expect("an open, readable stream")
    }

    #[test]
    fn a_file_stream_reads_its_file_then_ends() {
        crate::bootstrap();
        let dir = std::env::temp_dir().join(format!("rosemary-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let file = dir.join("contents");
        std::fs::write(&file, b"twelve bytes").expect("a file to read");
        let url = crate::url::new_file_url(file.to_str().expect("a UTF-8 path"), false);
        // SAFETY: a file URL this test made.
        let stream = unsafe { CFReadStreamCreateWithFile(std::ptr::null(), url) };
        assert!(!stream.is_null());
        // SAFETY: a stream this test made.
        unsafe {
            assert_eq!(CFReadStreamGetStatus(stream), Status::NotOpen as CFIndex);
            assert_eq!(CFReadStreamRead(stream, [0u8; 4].as_mut_ptr(), 4), -1);
            assert_eq!(CFReadStreamOpen(stream), 1);
        }
        assert_eq!(read_all(stream), b"twelve bytes");
        // SAFETY: as above.
        unsafe {
            assert_eq!(CFReadStreamGetStatus(stream), Status::AtEnd as CFIndex);
            CFReadStreamClose(stream);
            assert_eq!(CFReadStreamGetStatus(stream), Status::Closed as CFIndex);
            crate::api::CFRelease(stream);
            crate::api::CFRelease(url);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_fails_to_open_with_its_errno() {
        crate::bootstrap();
        let url = crate::url::new_file_url("/nonexistent/rosemary-stream", false);
        // SAFETY: a file URL this test made, and the stream made from it.
        unsafe {
            let stream = CFReadStreamCreateWithFile(std::ptr::null(), url);
            assert!(!stream.is_null(), "the file is only looked for at open");
            assert_eq!(CFReadStreamOpen(stream), 0);
            assert_eq!(CFReadStreamGetStatus(stream), Status::Error as CFIndex);
            let state = state(stream).expect("a stream");
            assert_eq!(state.errno, libc::ENOENT);
            crate::api::CFRelease(stream);
            crate::api::CFRelease(url);
        }
    }

    #[test]
    fn a_byte_stream_reads_in_pieces() {
        crate::bootstrap();
        let bytes = b"abcdefg";
        // SAFETY: a buffer described by its own length, with no deallocator.
        let stream = unsafe {
            CFReadStreamCreateWithBytesNoCopy(
                std::ptr::null(),
                bytes.as_ptr(),
                bytes.len() as CFIndex,
                crate::api::standard_allocator(crate::api::StandardAllocator::Null),
            )
        };
        let mut piece = [0u8; 3];
        // SAFETY: a stream this test made and a writable buffer.
        unsafe {
            assert_eq!(CFReadStreamOpen(stream), 1);
            assert_eq!(CFReadStreamRead(stream, piece.as_mut_ptr(), 3), 3);
            assert_eq!(&piece, b"abc");
        }
        assert_eq!(read_all(stream), b"defg");
        // SAFETY: as above.
        unsafe { crate::api::CFRelease(stream) };
    }

    #[test]
    fn a_property_list_reads_from_a_stream() {
        crate::bootstrap();
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Name</key><string>value</string></dict></plist>"#;
        // SAFETY: a buffer described by its own length, with no deallocator.
        let stream = unsafe {
            CFReadStreamCreateWithBytesNoCopy(
                std::ptr::null(),
                xml.as_ptr(),
                xml.len() as CFIndex,
                crate::api::standard_allocator(crate::api::StandardAllocator::Null),
            )
        };
        let mut format: CFIndex = 0;
        let mut error: CFTypeRef = std::ptr::null();
        // SAFETY: a stream this test made, and writable out-parameters.
        let list = unsafe {
            CFReadStreamOpen(stream);
            CFPropertyListCreateWithStream(std::ptr::null(), stream, 0, 0, &mut format, &mut error)
        };
        assert!(!list.is_null());
        assert!(error.is_null());
        // SAFETY: a dictionary the call made.
        assert_eq!(
            unsafe { crate::collections::dictionary(list) }.map(|d| d.len()),
            Some(1)
        );
        // SAFETY: both are ours.
        unsafe {
            crate::api::CFRelease(list);
            crate::api::CFRelease(stream);
        }
    }

    #[test]
    fn a_stream_is_an_input_stream() {
        crate::bootstrap();
        let input = objc::class::lookup(c"NSInputStream").expect("registered");
        assert!(class().chain().any(|k| std::ptr::eq(k, input)));
        // SAFETY: no-arguments class function.
        assert_eq!(
            unsafe { CFReadStreamGetTypeID() },
            crate::base::type_id(input)
        );
    }
}
