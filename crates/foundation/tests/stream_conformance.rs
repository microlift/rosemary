//! NSStream conformance: `TestStream.swift` from swift-corelibs-foundation
//! (swiftlang/swift-corelibs-foundation, `Tests/Foundation/`), Apple's
//! open-source Foundation and the one whose tests are published, ported case
//! by case and sent the messages its Swift sends, as a guest sends them.
//!
//! Classes bind as a guest's `_OBJC_CLASS_$_` imports do, from the data this
//! crate and CoreFoundation publish, and a message a class does not answer is
//! the exception it would be on a Mac: a failure.
//!
//! ```text
//! cargo test -p foundation --test stream_conformance -- --nocapture
//! ```
#![cfg(not(target_os = "macos"))]

#[path = "../../darwin/tests/conformance/mod.rs"]
mod conformance;

use std::ffi::c_void;
use std::path::PathBuf;

use conformance::{Case, Import, Kind, Stop, T, bind};
use objc::object::Id;
use objc::sel::Sel;

const UPSTREAM: &str = "swift-corelibs-foundation main Tests/Foundation/TestStream.swift";

const NS_UTF8_STRING_ENCODING: usize = 4;

// `NSStreamStatus`
const NOT_OPEN: usize = 0;
const OPEN: usize = 2;
const CLOSED: usize = 6;
const ERROR: usize = 7;

/// What a guest's import of `symbol` binds to, among Foundation's and
/// CoreFoundation's exports and then the system's.
fn address(symbol: &str) -> Option<u64> {
    let data = foundation::data_symbols()
        .into_iter()
        .chain(corefoundation::data_symbols())
        .find(|(name, _)| name == symbol)
        .map(|(_, addr)| addr);
    let bare = symbol.strip_prefix('_').unwrap_or(symbol);
    data.or_else(|| foundation::symbol(bare))
        .or_else(|| corefoundation::symbol(bare))
        .or_else(|| conformance::address(symbol))
}

fn import<F: Copy>(symbol: &'static str) -> Import<F> {
    bind(symbol, address(symbol))
}

/// The class a guest's `_OBJC_CLASS_$_<name>` import binds to.
fn class(symbol: &'static str) -> Result<Id, Stop> {
    let class: Import<Id> = import(symbol);
    class.get()
}

/// `objc_msgSend` as `F`, for `selector` sent to `receiver`, or the failure an
/// unrecognized selector is.
fn send<F: Copy>(receiver: Id, selector: &str) -> Result<(F, Sel), Stop> {
    assert_eq!(size_of::<F>(), size_of::<usize>());
    let sel = objc::sel::get(selector);
    if receiver.is_null() {
        return Err(Stop::Fail(format!(
            "-{selector} sent to nil, which upstream unwraps"
        )));
    }
    // SAFETY: a live object or class.
    let class = unsafe { objc::object::class_of(receiver) }.expect("a live object");
    if class.method(sel).is_none() {
        return Err(Stop::Fail(format!(
            "unrecognized selector -[{} {selector}]",
            // SAFETY: a class's name, which lives as long as it.
            unsafe { std::ffi::CStr::from_ptr(class.name()) }.to_string_lossy()
        )));
    }
    let entry = objc::msg::entry();
    // SAFETY: `objc_msgSend`, called as the method's signature, which `F` is.
    Ok((
        unsafe { std::mem::transmute_copy::<*const c_void, F>(&entry) },
        sel,
    ))
}

fn id(receiver: Id, selector: &str) -> Result<Id, Stop> {
    let (f, sel) = send::<unsafe extern "C" fn(Id, Sel) -> Id>(receiver, selector)?;
    Ok(unsafe { f(receiver, sel) })
}

fn id_with(receiver: Id, selector: &str, arg: Id) -> Result<Id, Stop> {
    let (f, sel) = send::<unsafe extern "C" fn(Id, Sel, Id) -> Id>(receiver, selector)?;
    Ok(unsafe { f(receiver, sel, arg) })
}

fn word(receiver: Id, selector: &str) -> Result<usize, Stop> {
    let (f, sel) = send::<unsafe extern "C" fn(Id, Sel) -> usize>(receiver, selector)?;
    Ok(unsafe { f(receiver, sel) })
}

fn boolean(receiver: Id, selector: &str) -> Result<bool, Stop> {
    let (f, sel) = send::<unsafe extern "C" fn(Id, Sel) -> bool>(receiver, selector)?;
    Ok(unsafe { f(receiver, sel) })
}

fn void(receiver: Id, selector: &str) -> Result<(), Stop> {
    let (f, sel) = send::<unsafe extern "C" fn(Id, Sel)>(receiver, selector)?;
    unsafe { f(receiver, sel) };
    Ok(())
}

/// `-read:maxLength:`
fn read(stream: Id, buffer: &mut [u8]) -> Result<isize, Stop> {
    let (f, sel) =
        send::<unsafe extern "C" fn(Id, Sel, *mut u8, usize) -> isize>(stream, "read:maxLength:")?;
    Ok(unsafe { f(stream, sel, buffer.as_mut_ptr(), buffer.len()) })
}

/// A Swift string literal as an `NSString`: `let message: NSString = "..."`.
fn literal(text: &str) -> Id {
    foundation::string::from_str(text)
}

/// `message.data(using: String.Encoding.utf8.rawValue)!`
fn utf8_data(message: Id) -> Result<Id, Stop> {
    let (f, sel) =
        send::<unsafe extern "C" fn(Id, Sel, usize) -> Id>(message, "dataUsingEncoding:")?;
    let data = unsafe { f(message, sel, NS_UTF8_STRING_ENCODING) };
    match data.is_null() {
        true => Err(Stop::Fail(
            "dataUsingEncoding: gave nil, which upstream unwraps".into(),
        )),
        false => Ok(data),
    }
}

/// `NSString(bytes: &buffer, length: buffer.firstIndex(of: 0) ?? buffer.count,
/// encoding: String.Encoding.utf8.rawValue)`
fn string_from(buffer: &[u8]) -> Result<Id, Stop> {
    let length = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    let alloc = id(class("_OBJC_CLASS_$_NSString")?, "alloc")?;
    let (f, sel) = send::<unsafe extern "C" fn(Id, Sel, *const u8, usize, usize) -> Id>(
        alloc,
        "initWithBytes:length:encoding:",
    )?;
    Ok(unsafe { f(alloc, sel, buffer.as_ptr(), length, NS_UTF8_STRING_ENCODING) })
}

/// `XCTAssertEqual(message, output!)`: `==` on two `NSString`s is `isEqual:`.
fn equal(a: Id, b: Id) -> Result<bool, Stop> {
    if b.is_null() {
        return Err(Stop::Fail(
            "NSString(bytes:length:encoding:) gave nil, which upstream unwraps".into(),
        ));
    }
    let (f, sel) = send::<unsafe extern "C" fn(Id, Sel, Id) -> bool>(a, "isEqual:")?;
    Ok(unsafe { f(a, sel, b) })
}

/// `NSTemporaryDirectory()`, as the guest sees it.
fn temporary_directory() -> Result<String, Stop> {
    let f: Import<unsafe extern "C" fn() -> Id> = import("_NSTemporaryDirectory");
    let dir = unsafe { f.get()?() };
    if dir.is_null() {
        return Err(Stop::Fail("NSTemporaryDirectory() gave nil".into()));
    }
    Ok(String::from_utf8_lossy(&unsafe { foundation::string::bytes(dir) }).into_owned())
}

/// `createTestFile(_:_contents:)`: a file in a new directory under
/// `NSTemporaryDirectory()`, and the guest path of it. Created on the host, as
/// it is scaffolding rather than what is under test.
struct TestFile {
    guest: String,
    dir: PathBuf,
}

impl TestFile {
    fn create(name: &str, contents: &[u8]) -> Result<TestFile, Stop> {
        let unique = format!(
            "TestFoundation_Playground_{}-{}/",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let guest_dir = temporary_directory()? + &unique;
        let dir = prefix::ns::host_path(&guest_dir);
        std::fs::create_dir(&dir)
            .and_then(|()| std::fs::write(dir.join(name), contents))
            .map_err(|_| Stop::Fail("Unable to create temp file".into()))?;
        Ok(TestFile {
            guest: guest_dir + name,
            dir,
        })
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Pool(*mut c_void);

impl Pool {
    fn new() -> Pool {
        foundation::bootstrap();
        Pool(objc::arc::pool_push())
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        objc::arc::pool_pop(self.0);
    }
}

/// The `if stream.hasBytesAvailable { ... }` the three reading cases share:
/// read, close, and check what came out. `count` is `messageData.count`,
/// which the URL and file cases compare the read with.
fn read_once(t: &mut T, stream: Id, message: Id, count: Option<usize>) -> Result<(), Stop> {
    let mut buffer = [0u8; 20];
    if !boolean(stream, "hasBytesAvailable")? {
        t.note("hasBytesAvailable was false after open, so upstream checks nothing more");
        return Ok(());
    }
    let result = read(stream, &mut buffer)?;
    void(stream, "close")?;
    let status = word(stream, "streamStatus")?;
    t.check(status == CLOSED, || {
        format!("streamStatus after close: {status}, not .closed")
    });
    if let Some(count) = count {
        t.check(result == count as isize, || {
            format!("read {result} bytes, not {count}")
        });
    }
    if result > 0 {
        let output = string_from(&buffer)?;
        t.check(equal(message, output)?, || {
            "the bytes read are not the message".into()
        });
    }
    Ok(())
}

fn status_is(t: &mut T, stream: Id, want: usize, name: &str) -> Result<(), Stop> {
    let status = word(stream, "streamStatus")?;
    t.check(status == want, || {
        format!("streamStatus {status}, not .{name}")
    });
    Ok(())
}

fn test_input_stream_with_data(t: &mut T) -> Result<(), Stop> {
    let _pool = Pool::new();
    let message = literal("Hello, playground");
    let message_data = utf8_data(message)?;
    let alloc = id(class("_OBJC_CLASS_$_NSInputStream")?, "alloc")?;
    let stream = id_with(alloc, "initWithData:", message_data)?;
    status_is(t, stream, NOT_OPEN, "notOpen")?;
    void(stream, "open")?;
    status_is(t, stream, OPEN, "open")?;
    read_once(t, stream, message, None)
}

fn test_input_stream_with_url(t: &mut T) -> Result<(), Stop> {
    let _pool = Pool::new();
    let message = literal("Hello, playground");
    let message_data = utf8_data(message)?;
    let count = word(message_data, "length")?;
    let file = TestFile::create("testFile_in.txt", b"Hello, playground")?;
    let url = id_with(
        class("_OBJC_CLASS_$_NSURL")?,
        "fileURLWithPath:",
        literal(&file.guest),
    )?;
    let alloc = id(class("_OBJC_CLASS_$_NSInputStream")?, "alloc")?;
    let stream = id_with(alloc, "initWithURL:", url)?;
    if stream.is_null() {
        return Err(Stop::Fail(
            "InputStream(url:) gave nil, which upstream unwraps".into(),
        ));
    }
    status_is(t, stream, NOT_OPEN, "notOpen")?;
    void(stream, "open")?;
    status_is(t, stream, OPEN, "open")?;
    read_once(t, stream, message, Some(count))
}

fn test_input_stream_with_file(t: &mut T) -> Result<(), Stop> {
    let _pool = Pool::new();
    let message = literal("Hello, playground");
    let message_data = utf8_data(message)?;
    let count = word(message_data, "length")?;
    let file = TestFile::create("testFile_in.txt", b"Hello, playground")?;
    let alloc = id(class("_OBJC_CLASS_$_NSInputStream")?, "alloc")?;
    let stream = id_with(alloc, "initWithFileAtPath:", literal(&file.guest))?;
    if stream.is_null() {
        return Err(Stop::Fail(
            "InputStream(fileAtPath:) gave nil, which upstream unwraps".into(),
        ));
    }
    status_is(t, stream, NOT_OPEN, "notOpen")?;
    void(stream, "open")?;
    status_is(t, stream, OPEN, "open")?;
    read_once(t, stream, message, Some(count))
}

fn test_input_stream_has_bytes_available(t: &mut T) -> Result<(), Stop> {
    let _pool = Pool::new();
    let message = literal("Hello, playground");
    let message_data = utf8_data(message)?;
    let alloc = id(class("_OBJC_CLASS_$_NSInputStream")?, "alloc")?;
    let stream = id_with(alloc, "initWithData:", message_data)?;
    let mut buffer = [0u8; 20];
    void(stream, "open")?;
    t.check(boolean(stream, "hasBytesAvailable")?, || {
        "hasBytesAvailable false after open".into()
    });
    read(stream, &mut buffer)?;
    t.check(!boolean(stream, "hasBytesAvailable")?, || {
        "hasBytesAvailable true after reading everything".into()
    });
    Ok(())
}

fn test_input_stream_invalid_path(t: &mut T) -> Result<(), Stop> {
    let _pool = Pool::new();
    let path = temporary_directory()? + "file.txt";
    if prefix::ns::host_path(&path).exists() {
        t.note(format!(
            "{path} exists here, which upstream takes it not to"
        ));
    }
    let alloc = id(class("_OBJC_CLASS_$_NSInputStream")?, "alloc")?;
    let stream = id_with(alloc, "initWithFileAtPath:", literal(&path))?;
    if stream.is_null() {
        return Err(Stop::Fail(
            "InputStream(fileAtPath:) gave nil, which upstream unwraps".into(),
        ));
    }
    status_is(t, stream, NOT_OPEN, "notOpen")?;
    void(stream, "open")?;
    status_is(t, stream, ERROR, "error")
}

fn test_input_stream_seek_to_position(_t: &mut T) -> Result<(), Stop> {
    Err(Stop::Skip(
        "Stream.seek(to:) is swift-corelibs-foundation's internal API, which the test builds only \
         with NS_FOUNDATION_ALLOWS_TESTABLE_IMPORT, and Darwin's Foundation has no such method"
            .into(),
    ))
}

/// The output stream cases, which each start by making one.
fn output_stream(_t: &mut T) -> Result<(), Stop> {
    let _pool = Pool::new();
    class("_OBJC_CLASS_$_NSOutputStream")?;
    Err(Stop::Fail(
        "NSOutputStream bound, and this port has not followed it further".into(),
    ))
}

const CASES: &[Case] = &[
    Case {
        name: "test_InputStreamWithData",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: test_input_stream_with_data,
    },
    Case {
        name: "test_InputStreamWithUrl",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: test_input_stream_with_url,
    },
    Case {
        name: "test_InputStreamWithFile",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: test_input_stream_with_file,
    },
    Case {
        name: "test_InputStreamHasBytesAvailable",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: test_input_stream_has_bytes_available,
    },
    Case {
        name: "test_InputStreamInvalidPath",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: test_input_stream_invalid_path,
    },
    Case {
        name: "test_InputStreamSeekToPosition",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: test_input_stream_seek_to_position,
    },
    Case {
        name: "test_outputStreamCreationToFile",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: output_stream,
    },
    Case {
        name: "test_outputStreamCreationToBuffer",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: output_stream,
    },
    Case {
        name: "test_outputStreamCreationWithUrl",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: output_stream,
    },
    Case {
        name: "test_outputStreamCreationToMemory",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: output_stream,
    },
    Case {
        name: "test_outputStreamHasSpaceAvailable",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: output_stream,
    },
    Case {
        name: "test_outputStreamWithInvalidPath",
        kind: Kind::Upstream,
        source: "TestStream.swift",
        timeout_secs: 30,
        run: output_stream,
    },
];

#[test]
fn stream_conformance() {
    conformance::run("stream_conformance", "NSStream", UPSTREAM, CASES);
}
