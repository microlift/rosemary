//! `NSStream` and `NSInputStream`.
//!
//! `NSStream` is abstract. Foundation's own streams are its subclasses, and so
//! is any stream a program writes - an upload's body stream, say - which
//! overrides the primitives. The base answers each of them as a stream that
//! was never opened would, so a subclass that leaves one alone gets that
//! answer rather than an unrecognized selector.
//!
//! `NSInputStream` is concrete for what its own initializers make: the bytes
//! of an `NSData`, or of a file, read in order. A program's subclass
//! allocates its own instances, which carry none of that state, and answers
//! the primitives itself.

use objc::object::Id;
use objc::sel::Sel;

use corefoundation::base::CFTypeRef;

use crate::boxed::{Body, contents, fill, make, register};
use crate::define::{class_methods, methods};

/// `NSStreamStatus`
const NOT_OPEN: usize = 0;
const OPEN: usize = 2;
const AT_END: usize = 5;
const CLOSED: usize = 6;
const ERROR: usize = 7;

/// `NSStreamFileCurrentOffsetKey`'s text, which is the key's own value.
const FILE_CURRENT_OFFSET_KEY: &str = "kCFStreamPropertyFileCurrentOffset";

/// Where an input stream's bytes come from.
enum Source {
    Bytes(Vec<u8>),
    /// A host path, read when the stream is opened, as a file stream finds
    /// out only then whether the file is there.
    File(std::path::PathBuf),
}

struct Input {
    source: Source,
    at: usize,
    status: usize,
    /// The `NSError` an open that failed left, retained.
    error: usize,
    /// Not retained, as a stream's delegate is not; zero is the stream itself.
    delegate: usize,
}

impl Drop for Input {
    fn drop(&mut self) {
        objc::arc::release(self.error as Id);
    }
}

impl Input {
    fn new(source: Source) -> Input {
        Input {
            source,
            at: 0,
            status: NOT_OPEN,
            error: 0,
            delegate: 0,
        }
    }

    fn bytes(&self) -> &[u8] {
        match &self.source {
            Source::Bytes(bytes) => bytes,
            Source::File(_) => &[],
        }
    }
}

fn stream_class() -> &'static objc::class::Class {
    corefoundation::base::abstract_class(c"NSStream", None)
}

fn input_class() -> &'static objc::class::Class {
    register::<Input>(c"NSInputStream", Some(stream_class()))
}

/// # Safety
///
/// `receiver` is null or an object this runtime allocated.
unsafe fn input<'a>(receiver: Id) -> Option<&'a mut Input> {
    // SAFETY: the caller's obligation.
    unsafe { contents::<Input>(receiver, input_class()) }
}

// The abstract primitives.

unsafe extern "C" fn nothing(_receiver: Id, _sel: Sel) {}

unsafe extern "C" fn nil(_receiver: Id, _sel: Sel) -> Id {
    std::ptr::null_mut()
}

unsafe extern "C" fn set_nothing(_receiver: Id, _sel: Sel, _value: Id) {}

unsafe extern "C" fn nil_for_key(_receiver: Id, _sel: Sel, _key: Id) -> Id {
    std::ptr::null_mut()
}

unsafe extern "C" fn refuse_property(_receiver: Id, _sel: Sel, _value: Id, _key: Id) -> bool {
    false
}

unsafe extern "C" fn schedule_nowhere(_receiver: Id, _sel: Sel, _loop: Id, _mode: Id) {}

unsafe extern "C" fn not_open(_receiver: Id, _sel: Sel) -> usize {
    NOT_OPEN
}

unsafe extern "C" fn no_buffer(
    _receiver: Id,
    _sel: Sel,
    _buffer: *mut *mut u8,
    _len: *mut usize,
) -> bool {
    false
}

// `NSInputStream`'s own.

/// `+inputStreamWithData:` and `-initWithData:`
fn from_data(data: Id) -> Option<Input> {
    // SAFETY: an `NSData`, or nil.
    let bytes = unsafe { corefoundation::collections::data(data as CFTypeRef) }?.clone();
    Some(Input::new(Source::Bytes(bytes)))
}

/// `+inputStreamWithFileAtPath:` and `-initWithFileAtPath:`
fn from_path(path: Id) -> Option<Input> {
    if path.is_null() {
        return None;
    }
    // SAFETY: an `NSString`.
    let path = String::from_utf8_lossy(&unsafe { crate::string::bytes(path) }).into_owned();
    Some(Input::new(Source::File(prefix::ns::host_path(&path))))
}

/// `+inputStreamWithURL:` and `-initWithURL:`, which a file URL is the one
/// kind of URL that names a stream's bytes for.
fn from_url(url: Id) -> Option<Input> {
    // SAFETY: an `NSURL`, or nil.
    let path = unsafe { corefoundation::url::as_url(url as CFTypeRef) }?.file_path()?;
    Some(Input::new(Source::File(prefix::ns::host_path(&path))))
}

fn made(input: Option<Input>) -> Id {
    match input {
        Some(input) => objc::arc::autorelease(make(input_class(), input)),
        None => std::ptr::null_mut(),
    }
}

/// An `-init...` on an allocated stream: filled, or released for nil.
fn initialized(receiver: Id, input: Option<Input>) -> Id {
    match input {
        // SAFETY: the receiver came from `+alloc` on an input stream class.
        Some(input) => {
            unsafe { fill(receiver, input) };
            receiver
        }
        None => {
            objc::arc::release(receiver);
            std::ptr::null_mut()
        }
    }
}

unsafe extern "C" fn stream_with_data(_class: Id, _sel: Sel, data: Id) -> Id {
    made(from_data(data))
}

unsafe extern "C" fn stream_with_path(_class: Id, _sel: Sel, path: Id) -> Id {
    made(from_path(path))
}

unsafe extern "C" fn stream_with_url(_class: Id, _sel: Sel, url: Id) -> Id {
    made(from_url(url))
}

unsafe extern "C" fn init_with_data(receiver: Id, _sel: Sel, data: Id) -> Id {
    initialized(receiver, from_data(data))
}

unsafe extern "C" fn init_with_path(receiver: Id, _sel: Sel, path: Id) -> Id {
    initialized(receiver, from_path(path))
}

unsafe extern "C" fn init_with_url(receiver: Id, _sel: Sel, url: Id) -> Id {
    initialized(receiver, from_url(url))
}

unsafe extern "C" fn open(receiver: Id, _sel: Sel) {
    // SAFETY: the receiver is a stream.
    let Some(input) = (unsafe { input(receiver) }) else {
        return;
    };
    if input.status != NOT_OPEN {
        return;
    }
    if let Source::File(path) = &input.source {
        match std::fs::read(path) {
            Ok(bytes) => input.source = Source::Bytes(bytes),
            Err(error) => {
                let code = error.raw_os_error().unwrap_or(libc::EIO) as i64;
                let error = crate::error::make("NSPOSIXErrorDomain", code, &error.to_string());
                input.error = objc::arc::retain(error) as usize;
                input.status = ERROR;
                return;
            }
        }
    }
    input.status = OPEN;
}

unsafe extern "C" fn close(receiver: Id, _sel: Sel) {
    // SAFETY: the receiver is a stream.
    if let Some(input) = unsafe { input(receiver) }
        && input.status != ERROR
    {
        input.status = CLOSED;
    }
}

unsafe extern "C" fn read(receiver: Id, _sel: Sel, buffer: *mut u8, len: usize) -> isize {
    // SAFETY: the receiver is a stream.
    let Some(input) = (unsafe { input(receiver) }) else {
        return -1;
    };
    match input.status {
        OPEN => {}
        AT_END => return 0,
        _ => return -1,
    }
    let rest = &input.bytes()[input.at.min(input.bytes().len())..];
    let n = rest.len().min(len);
    if n > 0 && !buffer.is_null() {
        // SAFETY: the caller's buffer holds `len` bytes, and `n` is no more.
        unsafe { std::ptr::copy_nonoverlapping(rest.as_ptr(), buffer, n) };
    }
    input.at += n;
    if input.at >= input.bytes().len() {
        input.status = AT_END;
    }
    n as isize
}

unsafe extern "C" fn has_bytes_available(receiver: Id, _sel: Sel) -> bool {
    // SAFETY: the receiver is a stream.
    unsafe { input(receiver) }.is_some_and(|input| input.status == OPEN)
}

unsafe extern "C" fn stream_status(receiver: Id, _sel: Sel) -> usize {
    // SAFETY: the receiver is a stream.
    unsafe { input(receiver) }.map_or(NOT_OPEN, |input| input.status)
}

unsafe extern "C" fn stream_error(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a stream.
    unsafe { input(receiver) }.map_or(std::ptr::null_mut(), |input| input.error as Id)
}

unsafe extern "C" fn delegate(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: the receiver is a stream.
    match unsafe { input(receiver) } {
        Some(input) if input.delegate != 0 => input.delegate as Id,
        Some(_) => receiver,
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn set_delegate(receiver: Id, _sel: Sel, delegate: Id) {
    // SAFETY: the receiver is a stream.
    if let Some(input) = unsafe { input(receiver) } {
        // A stream that is its own delegate is the default, set or not.
        input.delegate = match delegate == receiver {
            true => 0,
            false => delegate as usize,
        };
    }
}

/// The key's text, for comparing with the stream property keys.
///
/// # Safety
///
/// `key` is nil or a string.
unsafe fn key_text(key: Id) -> Option<String> {
    (!key.is_null())
        .then(|| String::from_utf8_lossy(&unsafe { crate::string::bytes(key) }).into_owned())
}

unsafe extern "C" fn property_for_key(receiver: Id, _sel: Sel, key: Id) -> Id {
    // SAFETY: the receiver is a stream, and the key a string.
    match (unsafe { input(receiver) }, unsafe { key_text(key) }) {
        (Some(input), Some(key)) if key == FILE_CURRENT_OFFSET_KEY => {
            crate::number(input.at as i64)
        }
        _ => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn set_property_for_key(receiver: Id, _sel: Sel, value: Id, key: Id) -> bool {
    // SAFETY: the receiver is a stream, and the key a string.
    let (Some(input), Some(key)) = (unsafe { input(receiver) }, unsafe { key_text(key) }) else {
        return false;
    };
    if key != FILE_CURRENT_OFFSET_KEY {
        return false;
    }
    // SAFETY: an `NSNumber`, or nil.
    let Some(offset) = (unsafe { crate::number_of(value) }).and_then(|n| usize::try_from(n).ok())
    else {
        return false;
    };
    input.at = offset;
    if input.status == AT_END && offset < input.bytes().len() {
        input.status = OPEN;
    }
    true
}

/// `NSStreamFileCurrentOffsetKey` and the other stream property keys a
/// program reads through their symbols.
pub const KEYS: &[(&str, &str)] = &[
    ("_NSStreamFileCurrentOffsetKey", FILE_CURRENT_OFFSET_KEY),
    (
        "_NSStreamDataWrittenToMemoryStreamKey",
        "kCFStreamPropertyDataWritten",
    ),
    (
        "_NSStreamSocketSecurityLevelKey",
        "kCFStreamPropertySocketSecurityLevel",
    ),
    (
        "_NSStreamSOCKSProxyConfigurationKey",
        "kCFStreamPropertySOCKSProxy",
    ),
    ("_NSStreamNetworkServiceType", "kCFStreamNetworkServiceType"),
];

pub(crate) fn install() {
    let stream = stream_class();
    methods!(stream, {
        "open" => nothing, c"v16@0:8";
        "close" => nothing, c"v16@0:8";
        "delegate" => nil, c"@16@0:8";
        "setDelegate:" => set_nothing, c"v24@0:8@16";
        "propertyForKey:" => nil_for_key, c"@24@0:8@16";
        "setProperty:forKey:" => refuse_property, c"B32@0:8@16@24";
        "scheduleInRunLoop:forMode:" => schedule_nowhere, c"v32@0:8@16@24";
        "removeFromRunLoop:forMode:" => schedule_nowhere, c"v32@0:8@16@24";
        "streamStatus" => not_open, c"Q16@0:8";
        "streamError" => nil, c"@16@0:8";
    });
    let input = input_class();
    debug_assert_eq!(input.instance_size(), size_of::<Body<Input>>());
    // A subclass that overrides none of these reaches them with no state of
    // its own, and each answers as for a stream that was never opened.
    methods!(input, {
        "read:maxLength:" => read, c"q32@0:8*16Q24";
        "getBuffer:length:" => no_buffer, c"B32@0:8^*16^Q24";
        "hasBytesAvailable" => has_bytes_available, c"B16@0:8";
        "open" => open, c"v16@0:8";
        "close" => close, c"v16@0:8";
        "streamStatus" => stream_status, c"Q16@0:8";
        "streamError" => stream_error, c"@16@0:8";
        "delegate" => delegate, c"@16@0:8";
        "setDelegate:" => set_delegate, c"v24@0:8@16";
        "propertyForKey:" => property_for_key, c"@24@0:8@16";
        "setProperty:forKey:" => set_property_for_key, c"B32@0:8@16@24";
        "initWithData:" => init_with_data, c"@24@0:8@16";
        "initWithFileAtPath:" => init_with_path, c"@24@0:8@16";
        "initWithURL:" => init_with_url, c"@24@0:8@16";
    });
    class_methods!(input, {
        "inputStreamWithData:" => stream_with_data, c"@24@0:8@16";
        "inputStreamWithFileAtPath:" => stream_with_path, c"@24@0:8@16";
        "inputStreamWithURL:" => stream_with_url, c"@24@0:8@16";
    });
}

/// Everything an input stream has left to give, read the way a consumer of
/// one does: opened, read until it reports the end, and closed. This is how
/// a request's body stream becomes the bytes it sends. `None` for a stream
/// that failed.
///
/// # Safety
///
/// `stream` is nil or a live `NSInputStream`, a program's subclass included.
pub unsafe fn drain(stream: Id) -> Option<Vec<u8>> {
    if stream.is_null() {
        return Some(Vec::new());
    }
    let send = |name: &str| {
        // SAFETY: `-open` and `-close` are `void (id, SEL)`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) = std::mem::transmute(objc::msg::entry());
            f(stream, objc::sel::get(name));
        }
    };
    send("open");
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    let read = objc::sel::get("read:maxLength:");
    let failed = loop {
        // SAFETY: `-read:maxLength:` is `NSInteger (id, SEL, uint8_t *,
        // NSUInteger)`, and the buffer holds what is offered.
        let n = unsafe {
            let f: unsafe extern "C" fn(Id, Sel, *mut u8, usize) -> isize =
                std::mem::transmute(objc::msg::entry());
            f(stream, read, chunk.as_mut_ptr(), chunk.len())
        };
        match n {
            0 => break false,
            n if n < 0 => break true,
            n => bytes.extend_from_slice(&chunk[..(n as usize).min(chunk.len())]),
        }
    };
    send("close");
    (!failed).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send<R>(receiver: Id, name: &str) -> R {
        // SAFETY: each use names a nullary method returning `R`.
        unsafe {
            let f: unsafe extern "C" fn(Id, Sel) -> R = std::mem::transmute(objc::msg::entry());
            f(receiver, objc::sel::get(name))
        }
    }

    #[test]
    fn a_data_stream_gives_its_bytes_and_then_the_end() {
        crate::bootstrap();
        let data = corefoundation::collections::new_data(b"twelve bytes".to_vec()) as Id;
        let class = objc::class::ptr(input_class()) as Id;
        // SAFETY: a class method taking an `NSData`.
        let stream =
            unsafe { stream_with_data(class, objc::sel::get("inputStreamWithData:"), data) };
        assert_eq!(send::<usize>(stream, "streamStatus"), NOT_OPEN);
        assert!(!send::<bool>(stream, "hasBytesAvailable"));
        // SAFETY: a live stream.
        assert_eq!(unsafe { drain(stream) }.unwrap(), b"twelve bytes");
        assert_eq!(send::<usize>(stream, "streamStatus"), CLOSED);
        // SAFETY: the data this test made.
        unsafe { corefoundation::api::CFRelease(data as CFTypeRef) };
    }

    #[test]
    fn a_file_that_is_not_there_fails_when_opened() {
        crate::bootstrap();
        let class = objc::class::ptr(input_class()) as Id;
        let path = crate::string::from_str("/nonexistent/rosemary-stream");
        // SAFETY: a class method taking an `NSString`.
        let stream =
            unsafe { stream_with_path(class, objc::sel::get("inputStreamWithFileAtPath:"), path) };
        assert!(!stream.is_null(), "made, and failing only when opened");
        send::<()>(stream, "open");
        assert_eq!(send::<usize>(stream, "streamStatus"), ERROR);
        assert!(!send::<Id>(stream, "streamError").is_null());
        // SAFETY: a live stream.
        assert!(unsafe { drain(stream) }.is_none());
        objc::arc::release(path);
    }
}
