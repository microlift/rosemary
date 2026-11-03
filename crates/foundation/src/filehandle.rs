use std::ffi::c_int;
use std::io::{Read, Seek, SeekFrom, Write};
use std::mem::ManuallyDrop;
use std::os::fd::{FromRawFd, IntoRawFd};

use objc::class::Class;
use objc::object::Id;
use objc::sel::Sel;

use corefoundation::base::CFTypeRef;

use crate::boxed::{contents, fill, make, register};
use crate::define::{class_methods, methods};

/// A descriptor, and whether this handle is the one that closes it.
struct Handle {
    /// -1 once closed.
    fd: c_int,
    close_on_dealloc: bool,
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.close_on_dealloc && self.fd >= 0 {
            // Release Darwin descriptor-associated state as well as the host FD.
            // SAFETY: a descriptor this handle owns.
            unsafe { darwin::net::descriptor::close(self.fd) };
        }
    }
}

fn handle_class() -> &'static Class {
    register::<Handle>(c"NSFileHandle", None)
}

/// # Safety
///
/// `object` must be null or an object this runtime allocated.
unsafe fn handle_of<'a>(object: Id) -> Option<&'a mut Handle> {
    unsafe { contents(object, handle_class()) }
}

/// The descriptor behind a file handle, or `None` for anything else.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
pub unsafe fn descriptor(object: Id) -> Option<c_int> {
    unsafe { handle_of(object) }
        .map(|handle| handle.fd)
        .filter(|fd| *fd >= 0)
}

/// Close a handle's descriptor, as `-closeFile` does.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
pub unsafe fn close(object: Id) {
    if let Some(handle) = unsafe { handle_of(object) } {
        if handle.fd >= 0 {
            // SAFETY: the handle's own descriptor, forgotten below.
            unsafe { darwin::net::descriptor::close(handle.fd) };
        }
        handle.fd = -1;
    }
}

fn autoreleased_handle(fd: c_int, close_on_dealloc: bool) -> Id {
    objc::arc::autorelease(make(
        handle_class(),
        Handle {
            fd,
            close_on_dealloc,
        },
    ))
}

/// The descriptor as a `File` that never closes it, for std's retrying I/O.
fn file(fd: c_int) -> ManuallyDrop<std::fs::File> {
    // SAFETY: the caller's live descriptor; `ManuallyDrop` leaves it open.
    ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Apple raises `NSFileHandleOperationException` here. There is no way to
/// raise from this side, so the failure is named and the caller gets nothing.
fn refuse(what: &str, error: &dyn std::fmt::Display) {
    eprintln!("rosemary: foundation: NSFileHandle {what} failed: {error}");
}

fn autoreleased_data(bytes: Vec<u8>) -> Id {
    let data = corefoundation::collections::new_data(bytes);
    // SAFETY: a fresh object.
    unsafe { corefoundation::api::CFAutorelease(data) as Id }
}

//

/// Open a guest path the way the three `+fileHandleFor...AtPath:` do: never
/// creating or truncating, and nil when there is nothing to open.
fn open_at(path: Id, read: bool, write: bool) -> Id {
    if path.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: an `NSString`.
    let guest = String::from_utf8_lossy(&unsafe { crate::string::bytes(path) }).into_owned();
    let opened = std::fs::OpenOptions::new()
        .read(read)
        .write(write)
        .open(prefix::ns::host_path(&guest));
    match opened {
        Ok(file) => autoreleased_handle(file.into_raw_fd(), true),
        Err(_) => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn for_reading_at_path(_class: Id, _sel: Sel, path: Id) -> Id {
    open_at(path, true, false)
}

unsafe extern "C" fn for_writing_at_path(_class: Id, _sel: Sel, path: Id) -> Id {
    open_at(path, false, true)
}

unsafe extern "C" fn for_updating_at_path(_class: Id, _sel: Sel, path: Id) -> Id {
    open_at(path, true, true)
}

unsafe extern "C" fn standard_input(_class: Id, _sel: Sel) -> Id {
    autoreleased_handle(0, false)
}

unsafe extern "C" fn standard_output(_class: Id, _sel: Sel) -> Id {
    autoreleased_handle(1, false)
}

unsafe extern "C" fn standard_error(_class: Id, _sel: Sel) -> Id {
    autoreleased_handle(2, false)
}

unsafe extern "C" fn null_device(_class: Id, _sel: Sel) -> Id {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
    {
        Ok(file) => autoreleased_handle(file.into_raw_fd(), true),
        Err(_) => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn init_with_descriptor(receiver: Id, _sel: Sel, fd: c_int) -> Id {
    // SAFETY: an instance of a class registered for `Handle`.
    unsafe {
        fill(
            receiver,
            Handle {
                fd,
                close_on_dealloc: false,
            },
        )
    };
    receiver
}

unsafe extern "C" fn init_with_descriptor_closing(
    receiver: Id,
    _sel: Sel,
    fd: c_int,
    close_on_dealloc: i8,
) -> Id {
    // SAFETY: an instance of a class registered for `Handle`.
    unsafe {
        fill(
            receiver,
            Handle {
                fd,
                close_on_dealloc: close_on_dealloc != 0,
            },
        )
    };
    receiver
}

unsafe extern "C" fn file_descriptor(receiver: Id, _sel: Sel) -> c_int {
    // SAFETY: a file handle.
    unsafe { handle_of(receiver) }.map_or(-1, |handle| handle.fd)
}

unsafe extern "C" fn read_to_end(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a file handle.
    let Some(fd) = (unsafe { descriptor(receiver) }) else {
        refuse("-readDataToEndOfFile", &"the handle is closed");
        return std::ptr::null_mut();
    };
    let mut bytes = Vec::new();
    if let Err(error) = (&*file(fd)).read_to_end(&mut bytes) {
        refuse("-readDataToEndOfFile", &error);
    }
    autoreleased_data(bytes)
}

/// `-readDataOfLength:`: up to `length` bytes, fewer only at end of file.
unsafe extern "C" fn read_of_length(receiver: Id, _sel: Sel, length: usize) -> Id {
    // SAFETY: a file handle.
    let Some(fd) = (unsafe { descriptor(receiver) }) else {
        refuse("-readDataOfLength:", &"the handle is closed");
        return std::ptr::null_mut();
    };
    let mut bytes = Vec::new();
    if let Err(error) = (&*file(fd)).take(length as u64).read_to_end(&mut bytes) {
        refuse("-readDataOfLength:", &error);
    }
    autoreleased_data(bytes)
}

/// `-availableData`: the rest of a regular file, or whatever one read of a
/// pipe or socket brings, which blocks until there is some or it closes.
unsafe extern "C" fn available_data(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a file handle.
    let Some(fd) = (unsafe { descriptor(receiver) }) else {
        refuse("-availableData", &"the handle is closed");
        return std::ptr::null_mut();
    };
    let handle = file(fd);
    let regular = handle.metadata().is_ok_and(|meta| meta.is_file());
    let mut bytes = Vec::new();
    let result = match regular {
        true => (&*handle).read_to_end(&mut bytes).map(drop),
        false => {
            bytes.resize(64 * 1024, 0);
            loop {
                match (&*handle).read(&mut bytes) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Ok(read) => {
                        bytes.truncate(read);
                        break Ok(());
                    }
                    Err(error) => {
                        bytes.clear();
                        break Err(error);
                    }
                }
            }
        }
    };
    if let Err(error) = result {
        refuse("-availableData", &error);
    }
    autoreleased_data(bytes)
}

unsafe extern "C" fn write_data(receiver: Id, _sel: Sel, data: Id) {
    // SAFETY: a file handle.
    let Some(fd) = (unsafe { descriptor(receiver) }) else {
        refuse("-writeData:", &"the handle is closed");
        return;
    };
    // SAFETY: an `NSData`, or nil.
    let Some(bytes) = (unsafe { corefoundation::collections::data(data as CFTypeRef) }) else {
        return;
    };
    if let Err(error) = (&*file(fd)).write_all(bytes) {
        refuse("-writeData:", &error);
    }
}

fn seek(receiver: Id, what: &str, to: SeekFrom) -> u64 {
    // SAFETY: a file handle.
    let Some(fd) = (unsafe { descriptor(receiver) }) else {
        refuse(what, &"the handle is closed");
        return 0;
    };
    (&*file(fd)).seek(to).unwrap_or_else(|error| {
        refuse(what, &error);
        0
    })
}

unsafe extern "C" fn seek_to_end(receiver: Id, _sel: Sel) -> u64 {
    seek(receiver, "-seekToEndOfFile", SeekFrom::End(0))
}

unsafe extern "C" fn seek_to_offset(receiver: Id, _sel: Sel, offset: u64) {
    seek(receiver, "-seekToFileOffset:", SeekFrom::Start(offset));
}

unsafe extern "C" fn offset_in_file(receiver: Id, _sel: Sel) -> u64 {
    seek(receiver, "-offsetInFile", SeekFrom::Current(0))
}

/// `-truncateFileAtOffset:`, which also leaves the file offset there.
unsafe extern "C" fn truncate_at(receiver: Id, _sel: Sel, offset: u64) {
    // SAFETY: a file handle.
    let Some(fd) = (unsafe { descriptor(receiver) }) else {
        refuse("-truncateFileAtOffset:", &"the handle is closed");
        return;
    };
    if let Err(error) = file(fd).set_len(offset) {
        refuse("-truncateFileAtOffset:", &error);
        return;
    }
    seek(receiver, "-truncateFileAtOffset:", SeekFrom::Start(offset));
}

unsafe extern "C" fn synchronize(receiver: Id, _sel: Sel) {
    // SAFETY: a file handle.
    if let Some(fd) = unsafe { descriptor(receiver) }
        && let Err(error) = file(fd).sync_all()
    {
        refuse("-synchronizeFile", &error);
    }
}

unsafe extern "C" fn close_file(receiver: Id, _sel: Sel) {
    // SAFETY: a file handle.
    unsafe { close(receiver) };
}

//

/// The two ends of a pipe, each a retained file handle.
struct Pipe {
    reading: Id,
    writing: Id,
}

impl Drop for Pipe {
    fn drop(&mut self) {
        objc::arc::release(self.reading);
        objc::arc::release(self.writing);
    }
}

fn pipe_class() -> &'static Class {
    register::<Pipe>(c"NSPipe", None)
}

/// The descriptor the child end of a pipe or file handle gives a spawned
/// process, and the handle the parent closes once it has: a pipe end left
/// open in the parent keeps its reader from ever seeing end of file.
///
/// `input` picks the pipe's reading end, as for a child's standard input.
///
/// # Safety
///
/// `object` must be null or an object this runtime allocated.
pub unsafe fn child_end(object: Id, input: bool) -> Option<(c_int, Option<Id>)> {
    if let Some(pipe) = unsafe { contents::<Pipe>(object, pipe_class()) } {
        let end = if input { pipe.reading } else { pipe.writing };
        return unsafe { descriptor(end) }.map(|fd| (fd, Some(end)));
    }
    unsafe { descriptor(object) }.map(|fd| (fd, None))
}

fn new_pipe() -> Option<Pipe> {
    let mut fds = [0; 2];
    // SAFETY: a writable pair.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    // Only a descriptor given to a child on purpose should reach it.
    for fd in fds {
        // SAFETY: descriptors just made.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    let end = |fd| {
        make(
            handle_class(),
            Handle {
                fd,
                close_on_dealloc: true,
            },
        )
    };
    Some(Pipe {
        reading: end(fds[0]),
        writing: end(fds[1]),
    })
}

unsafe extern "C" fn pipe(_class: Id, _sel: Sel) -> Id {
    match new_pipe() {
        Some(pipe) => objc::arc::autorelease(make(pipe_class(), pipe)),
        None => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn pipe_init(receiver: Id, _sel: Sel) -> Id {
    let Some(pipe) = new_pipe() else {
        objc::arc::release(receiver);
        return std::ptr::null_mut();
    };
    // SAFETY: an instance of a class registered for `Pipe`.
    unsafe { fill(receiver, pipe) };
    receiver
}

unsafe extern "C" fn for_reading(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a pipe.
    unsafe { contents::<Pipe>(receiver, pipe_class()) }
        .map_or(std::ptr::null_mut(), |pipe| pipe.reading)
}

unsafe extern "C" fn for_writing(receiver: Id, _sel: Sel) -> Id {
    // SAFETY: a pipe.
    unsafe { contents::<Pipe>(receiver, pipe_class()) }
        .map_or(std::ptr::null_mut(), |pipe| pipe.writing)
}

pub(crate) fn install() {
    let handle = handle_class();
    class_methods!(handle, {
        "fileHandleForReadingAtPath:" => for_reading_at_path, c"@24@0:8@16";
        "fileHandleForWritingAtPath:" => for_writing_at_path, c"@24@0:8@16";
        "fileHandleForUpdatingAtPath:" => for_updating_at_path, c"@24@0:8@16";
        "fileHandleWithStandardInput" => standard_input, c"@16@0:8";
        "fileHandleWithStandardOutput" => standard_output, c"@16@0:8";
        "fileHandleWithStandardError" => standard_error, c"@16@0:8";
        "fileHandleWithNullDevice" => null_device, c"@16@0:8";
    });
    methods!(handle, {
        "initWithFileDescriptor:" => init_with_descriptor, c"@20@0:8i16";
        "initWithFileDescriptor:closeOnDealloc:" => init_with_descriptor_closing, c"@24@0:8i16c20";
        "fileDescriptor" => file_descriptor, c"i16@0:8";
        "readDataToEndOfFile" => read_to_end, c"@16@0:8";
        "readDataOfLength:" => read_of_length, c"@24@0:8Q16";
        "availableData" => available_data, c"@16@0:8";
        "writeData:" => write_data, c"v24@0:8@16";
        "seekToEndOfFile" => seek_to_end, c"Q16@0:8";
        "seekToFileOffset:" => seek_to_offset, c"v24@0:8Q16";
        "offsetInFile" => offset_in_file, c"Q16@0:8";
        "truncateFileAtOffset:" => truncate_at, c"v24@0:8Q16";
        "synchronizeFile" => synchronize, c"v16@0:8";
        "closeFile" => close_file, c"v16@0:8";
    });

    let pipes = pipe_class();
    class_methods!(pipes, {
        "pipe" => pipe, c"@16@0:8";
    });
    methods!(pipes, {
        "init" => pipe_init, c"@16@0:8";
        "fileHandleForReading" => for_reading, c"@16@0:8";
        "fileHandleForWriting" => for_writing, c"@16@0:8";
    });
}

/// A new autoreleased `NSPipe`, for other modules' tests.
#[cfg(test)]
pub(crate) fn tests_pipe() -> Id {
    // SAFETY: `+pipe` takes no arguments.
    unsafe { pipe(std::ptr::null_mut(), objc::sel::get("pipe")) }
}

/// A pipe's writing and reading ends.
#[cfg(test)]
pub(crate) fn tests_ends(pipe: Id) -> (Id, Id) {
    // SAFETY: the accessors take no arguments.
    unsafe {
        (
            for_writing(pipe, objc::sel::get("fileHandleForWriting")),
            for_reading(pipe, objc::sel::get("fileHandleForReading")),
        )
    }
}

/// Everything left to read from a handle.
#[cfg(test)]
pub(crate) fn tests_read_all(handle: Id) -> Vec<u8> {
    // SAFETY: `-readDataToEndOfFile` takes no arguments, and answers `NSData`.
    unsafe {
        let data = read_to_end(handle, objc::sel::get("readDataToEndOfFile"));
        corefoundation::collections::data(data as CFTypeRef)
            .expect("data")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(bytes: &[u8]) -> Id {
        corefoundation::collections::new_data(bytes.to_vec()) as Id
    }

    fn bytes_of(data: Id) -> Vec<u8> {
        // SAFETY: an `NSData` this module made.
        unsafe { corefoundation::collections::data(data as CFTypeRef) }
            .expect("data")
            .clone()
    }

    #[test]
    fn a_pipe_carries_what_is_written_until_the_writer_closes() {
        crate::bootstrap();
        let pool = objc::arc::pool_push();
        // SAFETY: each is the declared prototype, on objects this module made.
        unsafe {
            let pipe = pipe(std::ptr::null_mut(), objc::sel::get("pipe"));
            let writer = for_writing(pipe, objc::sel::get("fileHandleForWriting"));
            let reader = for_reading(pipe, objc::sel::get("fileHandleForReading"));
            let message = data(b"hello");
            write_data(writer, objc::sel::get("writeData:"), message);
            close_file(writer, objc::sel::get("closeFile"));
            let read = read_to_end(reader, objc::sel::get("readDataToEndOfFile"));
            assert_eq!(bytes_of(read), b"hello");
            objc::arc::release(message);
        }
        objc::arc::pool_pop(pool);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn closing_filehandle_clears_darwin_socket_state() {
        crate::bootstrap();
        let pool = objc::arc::pool_push();
        let mut fds = [-1; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) },
            0
        );
        type SetSockOpt = unsafe extern "C" fn(i32, i32, i32, *const libc::c_void, u32) -> i32;
        let shim: SetSockOpt = unsafe {
            std::mem::transmute(darwin::symbol("_setsockopt").expect("Darwin setsockopt"))
        };
        let enabled = 1i32;
        assert_eq!(
            unsafe { shim(fds[0], 0xffff, 0x1022, (&raw const enabled).cast(), 4) },
            0
        );
        assert!(darwin::net::sockopt::suppresses_sigpipe(fds[0]));
        let file = autoreleased_handle(fds[0], true);
        unsafe { close_file(file, objc::sel::get("closeFile")) };
        assert_eq!(unsafe { libc::fcntl(fds[0], libc::F_GETFD) }, -1);
        assert!(!darwin::net::sockopt::suppresses_sigpipe(fds[0]));
        assert_eq!(unsafe { libc::close(fds[1]) }, 0);
        objc::arc::pool_pop(pool);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn filehandle_deallocation_clears_darwin_socket_state() {
        crate::bootstrap();
        let pool = objc::arc::pool_push();
        let mut fds = [-1; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) },
            0
        );
        type SetSockOpt = unsafe extern "C" fn(i32, i32, i32, *const libc::c_void, u32) -> i32;
        let shim: SetSockOpt = unsafe {
            std::mem::transmute(darwin::symbol("_setsockopt").expect("Darwin setsockopt"))
        };
        let enabled = 1i32;
        assert_eq!(
            unsafe { shim(fds[0], 0xffff, 0x1022, (&raw const enabled).cast(), 4) },
            0
        );
        assert!(darwin::net::sockopt::suppresses_sigpipe(fds[0]));
        let file = autoreleased_handle(fds[0], true);
        assert!(!file.is_null());
        // The last reference is released by the autorelease pool; the
        // destructor must follow the same descriptor lifecycle as -closeFile.
        objc::arc::pool_pop(pool);
        assert_eq!(unsafe { libc::fcntl(fds[0], libc::F_GETFD) }, -1);
        assert!(!darwin::net::sockopt::suppresses_sigpipe(fds[0]));
        assert_eq!(unsafe { libc::close(fds[1]) }, 0);
    }

    #[test]
    fn a_handle_for_a_missing_path_is_nil() {
        crate::bootstrap();
        let path = crate::string::from_str("/rosemary-no-such-directory/file");
        // SAFETY: `+fileHandleForReadingAtPath:` is `id (Class, SEL, id)`.
        let handle = unsafe {
            for_reading_at_path(
                std::ptr::null_mut(),
                objc::sel::get("fileHandleForReadingAtPath:"),
                path,
            )
        };
        assert!(handle.is_null());
        objc::arc::release(path);
    }
}
