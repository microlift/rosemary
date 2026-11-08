use std::ffi::{CStr, CString, c_void};
#[cfg(not(target_os = "macos"))]
use std::{
    ffi::{c_char, c_int},
    ptr,
};

/// The widest count a guest can ask for, in frames.
///
/// Darwin has no documented ceiling; this one is here so a garbage `size` -
/// which is what a corrupt crash handler passes - cannot turn into a
/// multi-gigabyte allocation in [`backtrace_symbols`].
const MAX_FRAMES: usize = 1 << 16;

#[cfg(not(target_os = "macos"))]
#[inline(never)]
pub unsafe extern "C" fn backtrace(array: *mut *mut c_void, size: c_int) -> c_int {
    if array.is_null() || size <= 0 {
        return 0;
    }
    let limit = (size as usize).min(MAX_FRAMES);
    let frames = walk(backtrace as *const () as usize, limit);
    let count = frames.len().min(limit);
    // SAFETY: the caller's promise, and `count` is capped at `size`.
    unsafe { ptr::copy_nonoverlapping(frames.as_ptr(), array, count) };
    count as c_int
}

#[cfg(not(target_os = "macos"))]
pub unsafe extern "C" fn backtrace_symbols(
    array: *const *mut c_void,
    size: c_int,
) -> *mut *mut c_char {
    if array.is_null() || size <= 0 {
        return ptr::null_mut();
    }
    let count = (size as usize).min(MAX_FRAMES);
    let lines: Vec<CString> = (0..count)
        // SAFETY: the caller's promise, and `count` is capped at `size`.
        .map(|i| describe(i, unsafe { *array.add(i) }))
        .collect();

    let table = count * size_of::<*mut c_char>();
    let text: usize = lines.iter().map(|l| l.as_bytes_with_nul().len()).sum();
    // SAFETY: a size, and the guest's own allocator - this block is one the
    // guest frees.
    let block = unsafe { crate::heap::malloc(table + text) } as *mut u8;
    if block.is_null() {
        return ptr::null_mut();
    }

    let entries = block as *mut *mut c_char;
    // SAFETY: inside the block just allocated, which is `table + text` bytes.
    let mut cursor = unsafe { block.add(table) };
    for (i, line) in lines.iter().enumerate() {
        let bytes = line.as_bytes_with_nul();
        // SAFETY: `text` is the sum of exactly these lengths, so the walk of
        // `cursor` stays inside the block, and `i < count`.
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), cursor, bytes.len());
            entries.add(i).write(cursor as *mut c_char);
            cursor = cursor.add(bytes.len());
        }
    }
    entries
}

#[cfg(not(target_os = "macos"))]
pub unsafe extern "C" fn backtrace_symbols_fd(array: *const *mut c_void, size: c_int, fd: c_int) {
    if array.is_null() || size <= 0 {
        return;
    }
    let count = (size as usize).min(MAX_FRAMES);
    for i in 0..count {
        // SAFETY: the caller's promise, and `count` is capped at `size`.
        let line = describe(i, unsafe { *array.add(i) });
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        write_all(fd, &bytes);
    }
}

/// The calling thread's return addresses, innermost first, from the caller of
/// `marker` outwards. See [`walk`] for what `marker` is.
pub fn return_addresses(marker: usize) -> Vec<usize> {
    walk(marker, MAX_FRAMES)
        .into_iter()
        .map(|frame| frame as usize)
        .collect()
}

/// The `backtrace_symbols` line for frame `index`, at `address`.
pub fn symbol_line(index: usize, address: usize) -> String {
    describe(index, address as *mut c_void)
        .to_string_lossy()
        .into_owned()
}

/// Walk the calling thread's stack, dropping the frames this crate's own
/// descent into the unwinder added.
///
/// `marker` is the address of the exported function the guest called. Every
/// frame up to and including it belongs to this runtime, so the collection is
/// cleared when it comes past and the guest's caller ends up at index 0. If it
/// never comes past - `_Unwind_FindEnclosingFunction` answering null, a host
/// whose unwinder cannot name an enclosing function - the frames are returned
/// as walked, with this crate's few at the front: a slightly wrong trace, not
/// an empty one.
fn walk(marker: usize, limit: usize) -> Vec<*mut c_void> {
    let mut frames: Vec<*mut c_void> = Vec::with_capacity(limit.min(64));
    let mut found = false;
    ::backtrace::trace(|frame| {
        if !found && frame.symbol_address() as usize == marker {
            found = true;
            frames.clear();
            return limit > 0;
        }
        frames.push(frame.ip());
        // Keep walking past the limit until the marker is behind us, because
        // until then none of what has been collected is the guest's.
        !found || frames.len() < limit
    });
    frames
}

/// One line of `backtrace_symbols` output for `addr`, in Darwin's layout:
/// `index`, image, address, symbol, offset from the symbol's start.
fn describe(index: usize, addr: *mut c_void) -> CString {
    let image = image_name(addr);
    let image = image.as_deref().unwrap_or("???");
    let line = match resolve_symbol(addr) {
        Some((name, start)) => format!(
            "{index:<4}{image:<35} 0x{:016x} {name} + {}",
            addr as usize,
            (addr as usize).saturating_sub(start)
        ),
        None => format!("{index:<4}{image:<35} 0x{:016x}", addr as usize),
    };
    // A resolved name is the only thing here that could carry an interior NUL,
    // and a symbol table holding one is corrupt rather than unusual - the
    // address still deserves a line.
    CString::new(line)
        .unwrap_or_else(|_| CString::new(format!("{index:<4}0x{:016x}", addr as usize)).unwrap())
}

/// The demangled name of the symbol covering `addr`, and where that symbol
/// starts.
///
/// `addr` is a return address, so it points *after* the call and at whatever
/// the compiler laid down next. That is the address Darwin reports and the one
/// sitting in the guest's array, so it is the one looked up - naming the call
/// site would print an address that is not in the trace.
fn resolve_symbol(addr: *mut c_void) -> Option<(String, usize)> {
    let mut found = None;
    // The first symbol is the innermost inlined frame, which is the most
    // specific answer to "what is at this address".
    ::backtrace::resolve(addr, |symbol| {
        if found.is_none()
            && let Some(name) = symbol.name()
        {
            let start = symbol.addr().map_or(addr as usize, |a| a as usize);
            found = Some((name.to_string(), start));
        }
    });
    found.or_else(|| {
        let info = dl_info(addr)?;
        if info.dli_sname.is_null() {
            return None;
        }
        // SAFETY: a non-null `dli_sname` is a NUL-terminated name the dynamic
        // loader owns, valid for as long as the image is loaded.
        let name = unsafe { CStr::from_ptr(info.dli_sname) }
            .to_string_lossy()
            .into_owned();
        Some((name, info.dli_saddr as usize))
    })
}

/// The base name of the image `addr` fell in.
///
/// `dladdr` rather than anything from the `backtrace` crate, which has no
/// public way to ask which object a frame came out of. It is POSIX-adjacent
/// and present on every host this crate builds for, unlike `<execinfo.h>`.
fn image_name(addr: *mut c_void) -> Option<String> {
    let info = dl_info(addr)?;
    if info.dli_fname.is_null() {
        return None;
    }
    // SAFETY: as `dli_sname` above - the loader owns the string.
    let path = unsafe { CStr::from_ptr(info.dli_fname) }.to_string_lossy();
    let base = path.rsplit('/').next().unwrap_or(&path);
    (!base.is_empty()).then(|| base.to_owned())
}

fn dl_info(addr: *mut c_void) -> Option<libc::Dl_info> {
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a writable `Dl_info`. `dladdr` tolerates an address
    // that is in no loaded image and answers zero for it.
    let ok = unsafe { libc::dladdr(addr, &mut info) };
    (ok != 0).then_some(info)
}

#[cfg(not(target_os = "macos"))]
/// `write` until the whole buffer is out, which is what a caller printing a
/// crash trace to a pipe needs and what a bare `write` does not promise.
fn write_all(fd: c_int, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: a descriptor the caller chose, and a buffer this process owns.
        let written = unsafe { libc::write(fd, bytes.as_ptr() as *const c_void, bytes.len()) };
        if written <= 0 {
            if written < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return;
        }
        bytes = &bytes[written as usize..];
    }
}

#[cfg(not(target_os = "macos"))]
exports::exports! {
    functions {
        "backtrace" => backtrace,
        "backtrace_symbols" => backtrace_symbols,
        "backtrace_symbols_fd" => backtrace_symbols_fd,
    }
}
