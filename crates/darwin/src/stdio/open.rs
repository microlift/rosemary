use std::ffi::{CStr, c_char, c_int};
use std::sync::Mutex;

use super::backend::{Mode, attach_fd};
use super::{EOF, File, Locked, engine, flag};

/// A descriptor as `_file` holds it, which is a `short`.
fn fits(fd: c_int) -> bool {
    fd <= i16::MAX as c_int
}

/// `FILE *fopen(const char *path, const char *mode)`
///
/// The file is opened by this crate's `open`, so it gets the guest's path
/// namespace, case folding and write checks along with it.
///
/// # Safety
///
/// Both arguments are NUL-terminated strings.
pub unsafe extern "C" fn fopen(path: *const c_char, mode: *const c_char) -> *mut File {
    // SAFETY: the caller's mode.
    let Some(mode) = (unsafe { Mode::parse(mode) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the caller's path; Darwin's flags for Darwin's `open`.
    let fd = unsafe { crate::stat::open(path, mode.darwin_open_flags(), 0o666) };
    if fd < 0 {
        return std::ptr::null_mut();
    }
    if !fits(fd) {
        // SAFETY: the descriptor just opened.
        unsafe { crate::net::descriptor::close(fd) };
        crate::errno::set(libc::EMFILE);
        return std::ptr::null_mut();
    }
    let fp = super::allocate();
    // SAFETY: a fresh stream. `ftell` after an append-mode open answers the
    // end, which `O_APPEND` alone would not until the first write.
    unsafe {
        attach_fd(fp, fd, mode.flags);
        if mode.append {
            super::state(fp).opened_append = true;
            engine::sseek(fp, 0, libc::SEEK_END);
        }
    }
    fp
}

/// `FILE *fdopen(int fd, const char *mode)`
///
/// # Safety
///
/// `mode` is a NUL-terminated string.
pub unsafe extern "C" fn fdopen(fd: c_int, mode: *const c_char) -> *mut File {
    if !fits(fd) {
        crate::errno::set(libc::EMFILE);
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's mode.
    let Some(mode) = (unsafe { Mode::parse(mode) }) else {
        return std::ptr::null_mut();
    };
    // SAFETY: an integer descriptor; the host's flags for the host's descriptor.
    let held = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if held < 0 {
        return std::ptr::null_mut();
    }
    let access = held & libc::O_ACCMODE;
    if access != libc::O_RDWR && access != mode.access {
        crate::errno::set(libc::EINVAL);
        return std::ptr::null_mut();
    }
    // SAFETY: as above.
    if mode.cloexec && unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return std::ptr::null_mut();
    }
    let fp = super::allocate();
    let mut flags = mode.flags;
    // A descriptor not opened for appending is appended to by seeking first.
    if mode.append && held & libc::O_APPEND == 0 {
        flags |= flag::APP;
    }
    // SAFETY: a fresh stream.
    unsafe { attach_fd(fp, fd, flags) };
    fp
}

/// `FILE *freopen(const char *path, const char *mode, FILE *stream)`
///
/// A stream on a descriptor keeps its descriptor number, so reopening
/// `stdout` really does redirect descriptor 1.
///
/// # Safety
///
/// `path` is null or a NUL-terminated string, `mode` is one, and `stream` is
/// a stream this module opened.
pub unsafe extern "C" fn freopen(
    path: *const c_char,
    mode: *const c_char,
    fp: *mut File,
) -> *mut File {
    if fp.is_null() {
        crate::errno::set(libc::EBADF);
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's mode.
    let Some(parsed) = (unsafe { Mode::parse(mode) }) else {
        // SAFETY: the caller's stream, which fails closed.
        unsafe { super::fclose(fp) };
        return std::ptr::null_mut();
    };
    // SAFETY: the caller's live stream, locked for the whole reopen.
    unsafe {
        let guard = Locked::new(fp);
        let (fd, wanted, was_open);
        if path.is_null() {
            // Only the mode changes, on the same descriptor.
            if (*fp).flags & flag::WR != 0 {
                engine::sflush(fp);
            }
            let fd_now = c_int::from((*fp).file);
            let held = libc::fcntl(fd_now, libc::F_GETFL);
            let access = held & libc::O_ACCMODE;
            if held < 0 || (access != libc::O_RDWR && access != parsed.access) {
                drop(guard);
                super::fclose(fp);
                crate::errno::set(libc::EBADF);
                return std::ptr::null_mut();
            }
            if (held & libc::O_APPEND != 0) != parsed.append {
                let toggled = match parsed.append {
                    true => held | libc::O_APPEND,
                    false => held & !libc::O_APPEND,
                };
                libc::fcntl(fd_now, libc::F_SETFL, toggled);
            }
            if parsed.truncate {
                libc::ftruncate(fd_now, 0);
            }
            if !parsed.append {
                engine::sseek(fp, 0, libc::SEEK_SET);
            }
            // The descriptor is kept, so nothing closes it below.
            (*fp).close = None;
            fd = fd_now;
            wanted = -1;
            was_open = false;
        } else {
            if (*fp).flags != 0 && (*fp).flags & flag::WR != 0 {
                engine::sflush(fp);
            }
            let mut open = (*fp).flags != 0 && (*fp).close.is_some();
            let keep = match open {
                true => c_int::from((*fp).file),
                false => -1,
            };
            let mut opened = crate::stat::open(path, parsed.darwin_open_flags(), 0o666);
            let error = crate::errno::get();
            // Out of descriptors: give back the old one and try again.
            if opened < 0 && open && matches!(error, libc::ENFILE | libc::EMFILE) {
                if let Some(close) = (*fp).close {
                    close((*fp).cookie);
                }
                open = false;
                opened = crate::stat::open(path, parsed.darwin_open_flags(), 0o666);
            } else {
                crate::errno::set(error);
            }
            fd = opened;
            wanted = keep;
            was_open = open;
        }
        let error = crate::errno::get();
        // The `dup2` below closes the old descriptor as it replaces it. Closing
        // it first would free its number for another thread's `open`, whose
        // descriptor the `dup2` would then close instead.
        let replaced = was_open && wanted >= 0 && fd >= 0 && wanted != fd;
        if was_open
            && !replaced
            && let Some(close) = (*fp).close
        {
            close((*fp).cookie);
        }
        engine::reset_buffers(fp);
        if fd < 0 {
            (*fp).flags = 0;
            drop(guard);
            crate::errno::set(error);
            return std::ptr::null_mut();
        }
        let mut fd = fd;
        if wanted >= 0 && wanted != fd {
            if crate::net::descriptor::dup2(fd, wanted) >= 0 {
                crate::net::descriptor::close(fd);
                fd = wanted;
            } else if replaced && let Some(close) = (*fp).close {
                close((*fp).cookie);
            }
        }
        if !fits(fd) {
            crate::net::descriptor::close(fd);
            (*fp).flags = 0;
            drop(guard);
            crate::errno::set(libc::EMFILE);
            return std::ptr::null_mut();
        }
        attach_fd(fp, fd, parsed.flags);
        if parsed.append {
            super::state(fp).opened_append = true;
            engine::sseek(fp, 0, libc::SEEK_END);
        }
        drop(guard);
        fp
    }
}

/// `int fclose(FILE *stream)`
///
/// # Safety
///
/// `stream` is a stream this module opened, not used again unless it is
/// one of the three static ones.
pub unsafe extern "C" fn fclose(fp: *mut File) -> c_int {
    // SAFETY: the caller's stream.
    unsafe {
        if fp.is_null() || (*fp).flags == 0 {
            crate::errno::set(libc::EBADF);
            return EOF;
        }
        let rc = {
            let _guard = Locked::new(fp);
            engine::teardown(fp)
        };
        super::release(fp);
        rc
    }
}

/// `FILE *tmpfile(void)`
///
/// An unlinked file in `/tmp`, made and removed through the guest-facing
/// calls so it lands where the guest's `/tmp` is.
pub unsafe extern "C" fn tmpfile() -> *mut File {
    let mut template = *b"/tmp/tmp.XXXXXXXXXX\0";
    // SAFETY: a writable template, NUL-terminated.
    let fd = unsafe { crate::fs::mkstemp(template.as_mut_ptr().cast()) };
    if fd < 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: the name `mkstemp` filled in.
    unsafe { crate::fs::unlink(template.as_ptr().cast()) };
    // SAFETY: the descriptor just made, and a constant mode.
    let fp = unsafe { fdopen(fd, c"w+".as_ptr()) };
    if fp.is_null() {
        let error = crate::errno::get();
        // SAFETY: the descriptor no stream took.
        unsafe { crate::net::descriptor::close(fd) };
        crate::errno::set(error);
    }
    fp
}

/// Streams `popen` opened, with their descriptor and the shell's pid.
static POPENED: Mutex<Vec<(usize, c_int, libc::pid_t)>> = Mutex::new(Vec::new());

/// `FILE *popen(const char *command, const char *mode)`
///
/// `r`, `w`, or Darwin's bidirectional `r+`, each optionally with `e`.
///
/// # Safety
///
/// Both arguments are NUL-terminated strings.
pub unsafe extern "C" fn popen(command: *const c_char, mode: *const c_char) -> *mut File {
    if command.is_null() || mode.is_null() {
        crate::errno::set(libc::EINVAL);
        return std::ptr::null_mut();
    }
    // SAFETY: the caller's two strings.
    let (command, mode) = unsafe { (CStr::from_ptr(command), CStr::from_ptr(mode).to_bytes()) };
    let cloexec = mode.contains(&b'e');
    let kind: &[u8] = match mode
        .iter()
        .filter(|&&b| b != b'e')
        .copied()
        .collect::<Vec<_>>()
    {
        m if m == b"r" => b"r",
        m if m == b"w" => b"w",
        m if m == b"r+" => b"r+",
        _ => {
            crate::errno::set(libc::EINVAL);
            return std::ptr::null_mut();
        }
    };
    let mut ends = [-1 as c_int; 2];
    // SAFETY: a writable pair of descriptors.
    let made = unsafe {
        match kind == b"r+" {
            true => libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, ends.as_mut_ptr()),
            false => libc::pipe(ends.as_mut_ptr()),
        }
    };
    if made < 0 {
        return std::ptr::null_mut();
    }
    // Neither end may outlive the exec on its own; the child's copy is the
    // `dup2`ed one, which does not inherit the flag.
    for fd in ends {
        // SAFETY: a descriptor just made.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    let (ours, theirs, onto): (c_int, c_int, &[c_int]) = match kind {
        b"r" => (ends[0], ends[1], &[1]),
        b"w" => (ends[1], ends[0], &[0]),
        _ => (ends[0], ends[1], &[0, 1]),
    };
    let dups: Vec<(c_int, c_int)> = onto.iter().map(|&to| (theirs, to)).collect();
    // POSIX: no stream an earlier `popen` opened is open in the new child.
    let earlier: Vec<c_int> = POPENED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|&(_, fd, _)| fd)
        .collect();
    let spawned = crate::spawn::spawn_shell(command, &dups, &earlier);
    // SAFETY: the child's end, which the parent no longer needs.
    unsafe { libc::close(theirs) };
    let pid = match spawned {
        Ok(pid) => pid,
        Err(error) => {
            // SAFETY: our end, which no stream took.
            unsafe { libc::close(ours) };
            crate::errno::set(error);
            return std::ptr::null_mut();
        }
    };
    if !cloexec {
        // SAFETY: our end.
        unsafe { libc::fcntl(ours, libc::F_SETFD, 0) };
    }
    let stream_mode = match kind {
        b"r" => c"r",
        b"w" => c"w",
        _ => c"r+",
    };
    // SAFETY: our end and a constant mode.
    let fp = unsafe { fdopen(ours, stream_mode.as_ptr()) };
    if fp.is_null() {
        // SAFETY: our end; the shell is reaped so it does not linger.
        unsafe { libc::close(ours) };
        let _ = crate::spawn::wait_child(pid, true);
        return std::ptr::null_mut();
    }
    POPENED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((fp as usize, ours, pid));
    fp
}

/// `int pclose(FILE *stream)`: the shell's wait status.
///
/// # Safety
///
/// `stream` came from [`popen`] and is not used again.
pub unsafe extern "C" fn pclose(fp: *mut File) -> c_int {
    let pid = {
        let mut open = POPENED.lock().unwrap_or_else(|e| e.into_inner());
        let Some(at) = open.iter().position(|&(f, _, _)| f == fp as usize) else {
            crate::errno::set(libc::ECHILD);
            return -1;
        };
        open.remove(at).2
    };
    // SAFETY: the caller's stream.
    unsafe { super::fclose(fp) };
    match crate::spawn::wait_child(pid, true) {
        Ok(Some(status)) => status,
        Ok(None) => -1,
        Err(error) => {
            crate::errno::set(error);
            -1
        }
    }
}
