use std::cell::UnsafeCell;
use std::ffi::{c_char, c_int, c_short, c_void};
use std::sync::{Condvar, Mutex};

mod api;
mod backend;
mod engine;
mod format;
mod open;
mod parseln;
mod wide;

pub use api::*;
pub use backend::{fmemopen, funopen, open_memstream, open_wmemstream};
pub use format::*;
pub use open::{fclose, fdopen, fopen, freopen, pclose, popen, tmpfile};
pub use parseln::fparseln;
pub use wide::*;

pub type ReadFn = unsafe extern "C" fn(*mut c_void, *mut c_char, c_int) -> c_int;
pub type WriteFn = unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> c_int;
pub type SeekFn = unsafe extern "C" fn(*mut c_void, i64, c_int) -> i64;
pub type CloseFn = unsafe extern "C" fn(*mut c_void) -> c_int;

/// `struct __sbuf`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Sbuf {
    pub base: *mut u8,
    pub size: c_int,
}

impl Sbuf {
    const EMPTY: Sbuf = Sbuf {
        base: std::ptr::null_mut(),
        size: 0,
    };
}

/// Darwin's `struct __sFILE`, field for field: guests inline code that reads
/// `_p`, `_r`, `_w`, `_flags`, `_file` and `_lbfsize` at these offsets.
#[repr(C)]
pub struct File {
    /// Current position in the buffer.
    pub p: *mut u8,
    /// Bytes left for `getc`.
    pub r: c_int,
    /// Room left for `putc`.
    pub w: c_int,
    pub flags: c_short,
    /// The descriptor, or -1.
    pub file: c_short,
    pub bf: Sbuf,
    /// 0, or `-bf.size` while line buffered, for the inlined `putc`.
    pub lbfsize: c_int,
    pub cookie: *mut c_void,
    pub close: Option<CloseFn>,
    pub read: Option<ReadFn>,
    pub seek: Option<SeekFn>,
    pub write: Option<WriteFn>,
    /// The `ungetc` buffer, when it is not `ubuf`.
    pub ub: Sbuf,
    pub extra: *mut Extra,
    /// `r` as it was before `ungetc` took over.
    pub ur: c_int,
    pub ubuf: [u8; 3],
    pub nbuf: [u8; 1],
    /// `fgetln`'s line buffer.
    pub lb: Sbuf,
    pub blksize: c_int,
    pub offset: i64,
}

const _: () = {
    assert!(size_of::<File>() == 152);
    assert!(std::mem::offset_of!(File, flags) == 16);
    assert!(std::mem::offset_of!(File, file) == 18);
    assert!(std::mem::offset_of!(File, lbfsize) == 40);
    assert!(std::mem::offset_of!(File, extra) == 104);
    assert!(std::mem::offset_of!(File, offset) == 144);
};

/// `_flags` bits, Darwin's values.
pub mod flag {
    use std::ffi::c_short;
    /// Line buffered.
    pub const LBF: c_short = 0x0001;
    /// Unbuffered.
    pub const NBF: c_short = 0x0002;
    /// Reading now.
    pub const RD: c_short = 0x0004;
    /// Writing now.
    pub const WR: c_short = 0x0008;
    /// Open for both.
    pub const RW: c_short = 0x0010;
    pub const EOF: c_short = 0x0020;
    pub const ERR: c_short = 0x0040;
    /// The buffer is `malloc`ed here.
    pub const MBF: c_short = 0x0080;
    /// Append: every write seeks to the end first.
    pub const APP: c_short = 0x0100;
    /// A regular file whose buffer matches its block size.
    pub const OPT: c_short = 0x0400;
    /// Not a candidate for `OPT`.
    pub const NPT: c_short = 0x0800;
    /// `offset` is accurate.
    pub const OFF: c_short = 0x1000;
    /// `fgetln` handed out a pointer into the buffer.
    pub const MOD: c_short = 0x2000;
    /// Skipped by the line-buffered flush a read triggers.
    pub const IGN: c_short = 0x8000u16 as c_short;
}

/// Darwin's `BUFSIZ`.
pub const BUFSIZ: c_int = 1024;
pub const EOF: c_int = -1;

/// What Darwin keeps behind `_extra`: the parts no guest inlines.
pub struct Extra {
    lock: Lock,
    state: UnsafeCell<State>,
}

// SAFETY: `state` is only touched by the thread holding `lock`.
unsafe impl Sync for Extra {}

pub(crate) struct State {
    /// `p` as it was before `ungetc` took over.
    pub up: *mut u8,
    /// `fwide`'s answer: negative for bytes, positive for wide characters.
    pub orientation: i8,
    /// The conversion state wide I/O carries between calls; big enough for
    /// any host's `mbstate_t`.
    pub mbstate: [u64; 16],
    /// `fgetwln`'s line.
    pub wide_line: Vec<u32>,
    /// Opened with `O_APPEND`: the kernel appends, but `ftell` must still
    /// ask where the end is. BSD's `__S2OAP`.
    pub opened_append: bool,
}

impl Extra {
    const fn new() -> Extra {
        Extra {
            lock: Lock::new(),
            state: UnsafeCell::new(State {
                up: std::ptr::null_mut(),
                orientation: 0,
                mbstate: [0; 16],
                wide_line: Vec::new(),
                opened_append: false,
            }),
        }
    }
}

/// The recursive lock `flockfile` takes and every function below takes too.
struct Lock {
    held: Mutex<(usize, u32)>,
    freed: Condvar,
}

impl Lock {
    const fn new() -> Lock {
        Lock {
            held: Mutex::new((0, 0)),
            freed: Condvar::new(),
        }
    }

    fn lock(&self) {
        let me = thread_token();
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        while held.0 != 0 && held.0 != me {
            held = self.freed.wait(held).unwrap_or_else(|e| e.into_inner());
        }
        held.0 = me;
        held.1 += 1;
    }

    fn try_lock(&self) -> bool {
        let me = thread_token();
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if held.0 != 0 && held.0 != me {
            return false;
        }
        held.0 = me;
        held.1 += 1;
        true
    }

    fn unlock(&self) {
        let me = thread_token();
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        // Unlocking a stream this thread does not hold is the caller's bug;
        // ignoring it beats handing another thread's lock away.
        if held.0 != me || held.1 == 0 {
            return;
        }
        held.1 -= 1;
        if held.1 == 0 {
            held.0 = 0;
            self.freed.notify_one();
        }
    }
}

/// A nonzero number unique to the calling thread while it lives.
fn thread_token() -> usize {
    thread_local! {
        static TOKEN: u8 = const { 0 };
    }
    TOKEN.with(|t| t as *const u8 as usize)
}

/// `fp`'s extra state.
///
/// # Safety
///
/// `fp` is a live stream and the caller holds its lock.
pub(crate) unsafe fn state<'a>(fp: *mut File) -> &'a mut State {
    // SAFETY: the caller's obligations; the lock is what makes the access
    // exclusive.
    unsafe { &mut *(*(*fp).extra).state.get() }
}

/// The lock held for a scope.
pub(crate) struct Locked(*mut File);

impl Locked {
    /// # Safety
    ///
    /// `fp` is a live stream.
    pub(crate) unsafe fn new(fp: *mut File) -> Locked {
        // SAFETY: the caller's obligation.
        unsafe { (*(*fp).extra).lock.lock() };
        Locked(fp)
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        // SAFETY: `new` locked a live stream, which `fclose` cannot free while
        // this guard is held.
        unsafe { (*(*self.0).extra).lock.unlock() };
    }
}

/// The three static streams, which Darwin exports as `__sF`.
#[repr(transparent)]
pub struct Streams(UnsafeCell<[File; 3]>);

// SAFETY: each stream is guarded by its own lock.
unsafe impl Sync for Streams {}

static STD_EXTRA: [Extra; 3] = [Extra::new(), Extra::new(), Extra::new()];

const fn std_stream(fd: c_short, flags: c_short) -> File {
    File {
        p: std::ptr::null_mut(),
        r: 0,
        w: 0,
        flags,
        file: fd,
        bf: Sbuf::EMPTY,
        lbfsize: 0,
        cookie: SF.0.get().cast::<File>().wrapping_add(fd as usize).cast(),
        close: Some(backend::fd_close),
        read: Some(backend::fd_read),
        seek: Some(backend::fd_seek),
        write: Some(backend::fd_write),
        ub: Sbuf::EMPTY,
        extra: (&raw const STD_EXTRA[fd as usize]).cast_mut(),
        ur: 0,
        ubuf: [0; 3],
        nbuf: [0; 1],
        lb: Sbuf::EMPTY,
        blksize: 0,
        offset: 0,
    }
}

pub static SF: Streams = Streams(UnsafeCell::new([
    std_stream(0, flag::RD),
    std_stream(1, flag::WR),
    std_stream(2, flag::WR | flag::NBF),
]));

/// `stdin`, `stdout` and `stderr`, which Darwin exports as pointer variables
/// a guest may also reassign.
#[repr(transparent)]
pub struct StreamPointer(UnsafeCell<*mut File>);

// SAFETY: a pointer-sized cell the guest owns, as it does in libc.
unsafe impl Sync for StreamPointer {}

impl StreamPointer {
    const fn new(fd: usize) -> StreamPointer {
        StreamPointer(UnsafeCell::new(SF.0.get().cast::<File>().wrapping_add(fd)))
    }

    fn get(&self) -> *mut File {
        // SAFETY: an aligned pointer cell; a guest's assignment races only with
        // its own reads.
        unsafe { *self.0.get() }
    }
}

pub static STDINP: StreamPointer = StreamPointer::new(0);
pub static STDOUTP: StreamPointer = StreamPointer::new(1);
pub static STDERRP: StreamPointer = StreamPointer::new(2);

/// The streams `fopen` and friends allocated, for `fflush(NULL)` and exit.
static OPEN: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// A fresh stream, registered.
pub(crate) fn allocate() -> *mut File {
    let extra = Box::into_raw(Box::new(Extra::new()));
    let fp = Box::into_raw(Box::new(File {
        p: std::ptr::null_mut(),
        r: 0,
        w: 0,
        flags: 0,
        file: -1,
        bf: Sbuf::EMPTY,
        lbfsize: 0,
        cookie: std::ptr::null_mut(),
        close: None,
        read: None,
        seek: None,
        write: None,
        ub: Sbuf::EMPTY,
        extra,
        ur: 0,
        ubuf: [0; 3],
        nbuf: [0; 1],
        lb: Sbuf::EMPTY,
        blksize: 0,
        offset: 0,
    }));
    OPEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(fp as usize);
    fp
}

/// Whether `fp` is one of the static streams, which are never freed.
fn is_std(fp: *mut File) -> bool {
    let base = SF.0.get().cast::<File>();
    (base..base.wrapping_add(3)).contains(&fp)
}

/// Give back what [`allocate`] made. The static streams stay.
///
/// # Safety
///
/// `fp` is closed and nothing else holds it.
pub(crate) unsafe fn release(fp: *mut File) {
    if is_std(fp) {
        return;
    }
    OPEN.lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|&open| open != fp as usize);
    // SAFETY: both came from `Box::into_raw` in `allocate`, and the stream is
    // out of the registry so nothing reaches it again.
    unsafe {
        drop(Box::from_raw((*fp).extra));
        drop(Box::from_raw(fp));
    }
}

/// Every open stream, the static ones first.
pub(crate) fn every_stream() -> Vec<*mut File> {
    let base = SF.0.get().cast::<File>();
    let mut all: Vec<*mut File> = (0..3).map(|i| base.wrapping_add(i)).collect();
    all.extend(
        OPEN.lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|&fp| fp as *mut File),
    );
    all
}

/// Flush every buffer at `exit`, as libc's `_cleanup` does.
///
/// Darwin flushes after every `atexit` handler has run, and a handler may
/// print. The host runs handlers last-registered first, so this must be
/// registered before any guest handler is: callers that register one call
/// this first. Idempotent.
pub fn register_cleanup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // Not `fflush(NULL)`: a stream another thread holds at exit is
        // skipped rather than waited on forever.
        extern "C" fn cleanup() {
            api::flush_all(false);
        }
        // SAFETY: a plain `extern "C"` function with no captures.
        unsafe { libc::atexit(cleanup) };
    });
}

/// The guest's current `stdin`, for a shim that reads what the guest would.
pub fn stdin() -> *mut c_void {
    current(&STDINP, "__stdinp")
}

/// The guest's current `stdout`, for this runtime's own output.
///
/// The builtin libc answers with this module's stream. The native one - macOS
/// only - with the host's, which is what a guest bound there too.
pub fn stdout() -> *mut c_void {
    current(&STDOUTP, "__stdoutp")
}

/// `stderr`, for the same reason.
pub fn stderr() -> *mut c_void {
    current(&STDERRP, "__stderrp")
}

fn current(ours: &StreamPointer, host: &str) -> *mut c_void {
    if crate::libc_backend::using_builtin() {
        return ours.get().cast();
    }
    match crate::host_symbol(host) {
        // SAFETY: Darwin's libc exports this as a `FILE *` variable.
        Some(at) => unsafe { *(at as usize as *const *mut c_void) },
        None => std::ptr::null_mut(),
    }
}

/// Write `bytes` to a guest's stream, for a shim that formatted them.
///
/// The stream is whichever kind the guest was given: this module's under the
/// builtin libc, the host's under the native one.
///
/// # Safety
///
/// `stream` is null or a stream the guest's libc opened.
pub unsafe fn write(stream: *mut c_void, bytes: &[u8]) -> usize {
    if stream.is_null() || bytes.is_empty() {
        return 0;
    }
    // SAFETY: the caller's stream, of the kind the selected libc made.
    unsafe {
        match crate::libc_backend::using_builtin() {
            true => fwrite(bytes.as_ptr().cast(), 1, bytes.len(), stream.cast()),
            false => libc::fwrite(bytes.as_ptr().cast(), 1, bytes.len(), stream.cast()),
        }
    }
}

/// Write wide characters to a guest's stream, as `fwprintf` does, for a
/// shim that rendered them. False if the stream refused any.
///
/// # Safety
///
/// `stream` is one this module opened.
pub unsafe fn write_wide(stream: *mut c_void, text: &[u32]) -> bool {
    let fp = stream.cast::<File>();
    // SAFETY: the caller's stream.
    unsafe {
        let _guard = Locked::new(fp);
        engine::orient(fp, 1);
        wide::write_wide(fp, text)
    }
}

/// Names a guest may bind that this module does not provide, so they must not
/// fall through to the host's stdio, which would be handed one of these
/// streams.
pub const REFUSED: &[&str] = &[
    "fwscanf",
    "wscanf",
    "vfwscanf",
    "vwscanf",
    "fwscanf_l",
    "wscanf_l",
    "vfwscanf_l",
    "vwscanf_l",
    "fwprintf_l",
    "wprintf_l",
    "vfwprintf_l",
    "vwprintf_l",
    "__svfscanf",
    "zopen",
];

exports::exports! {
    functions {
        "fopen" | "fopen$DARWIN_EXTSN" => fopen,
        "fdopen" | "fdopen$DARWIN_EXTSN" => fdopen,
        "freopen" => freopen,
        "fclose" => fclose,
        "tmpfile" => tmpfile,
        "popen" => popen,
        "pclose" => pclose,
        "funopen" => funopen,
        "fmemopen" => fmemopen,
        "open_memstream" => open_memstream,
        "open_wmemstream" => open_wmemstream,

        "fread" => fread,
        "fwrite" => fwrite,
        "fgetc" => fgetc,
        "getc" => getc,
        "getc_unlocked" => getc_unlocked,
        "getchar" => getchar,
        "getchar_unlocked" => getchar_unlocked,
        "fputc" => fputc,
        "putc" => putc,
        "putc_unlocked" => putc_unlocked,
        "putchar" => putchar,
        "putchar_unlocked" => putchar_unlocked,
        "fgets" => fgets,
        "gets" => gets,
        "fputs" => fputs,
        "puts" => puts,
        "ungetc" => ungetc,
        "getw" => getw,
        "putw" => putw,
        "getline" => getline,
        "getdelim" => getdelim,
        "fgetln" => fgetln,
        "fparseln" => fparseln,
        "perror" => perror,

        "fflush" => fflush,
        "fpurge" | "__fpurge" => fpurge,
        "fseek" => fseek,
        "fseeko" => fseeko,
        "ftell" => ftell,
        "ftello" => ftello,
        "rewind" => rewind,
        "fgetpos" => fgetpos,
        "fsetpos" => fsetpos,

        "feof" => feof,
        "feof_unlocked" => feof_unlocked,
        "ferror" => ferror,
        "ferror_unlocked" => ferror_unlocked,
        "clearerr" => clearerr,
        "clearerr_unlocked" => clearerr_unlocked,
        "fileno" => fileno,
        "fileno_unlocked" => fileno_unlocked,

        "setbuf" => setbuf,
        "setvbuf" => setvbuf,
        "setbuffer" => setbuffer,
        "setlinebuf" => setlinebuf,

        "flockfile" => flockfile,
        "funlockfile" => funlockfile,
        "ftrylockfile" => ftrylockfile,

        "__srget" => __srget,
        "__swbuf" => __swbuf,

        "fgetwc" => fgetwc,
        "getwc" => getwc,
        "getwchar" => getwchar,
        "fputwc" => fputwc,
        "putwc" => putwc,
        "putwchar" => putwchar,
        "ungetwc" => ungetwc,
        "fgetws" => fgetws,
        "fputws" => fputws,
        "fgetwln" => fgetwln,
        "fwide" => fwide,

        "printf" => printf,
        "fprintf" => fprintf,
        "vprintf" => vprintf,
        "vfprintf" => vfprintf,
        "printf_l" => printf_l,
        "fprintf_l" => fprintf_l,
        "vprintf_l" => vprintf_l,
        "vfprintf_l" => vfprintf_l,
        "scanf" => scanf,
        "fscanf" => fscanf,
        "vscanf" => vscanf,
        "vfscanf" => vfscanf,
        "scanf_l" => scanf_l,
        "fscanf_l" => fscanf_l,
        "vscanf_l" => vscanf_l,
        "vfscanf_l" => vfscanf_l,
        "wprintf" => wprintf,
        "fwprintf" => fwprintf,
        "vwprintf" => vwprintf,
        "vfwprintf" => vfwprintf,
    }
    // The Mach-O underscore is added here; the C identifiers carry one of their
    // own, so the guest imports `___stdoutp`.
    constants {
        "__sF" => SF.0.get() as u64,
        "__stdinp" => STDINP.0.get() as u64,
        "__stdoutp" => STDOUTP.0.get() as u64,
        "__stderrp" => STDERRP.0.get() as u64,
    }
}

#[cfg(test)]
mod tests;
