use std::ffi::{c_char, c_void};
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

use machish::resolve::SyntheticDylib;

/// The guest's stack canary.
static STACK_CHK_GUARD: u64 = 0x00ff_dead_beef_c0de;

/// The guest's `argc`, as `_NSGetArgc` hands out the address of.
static GUEST_ARGC: AtomicI32 = AtomicI32::new(0);

/// The guest's `argv`, as `_NSGetArgv` hands out the address of.
static GUEST_ARGV: AtomicPtr<*mut c_char> = AtomicPtr::new(std::ptr::null_mut());

/// Record the arguments the guest was entered with.
///
/// Called once the stack is laid out, which is after the chain that answers these
/// imports was built - so the two accessors below hand out the addresses of these
/// statics and read them when the guest calls, rather than at bind time.
///
/// The guest's arguments, not this process's: `argv[0]` has to be the program the
/// guest thinks it is, not `rosetta3`.
pub fn set_process_args(argc: u64, argv: u64) {
    GUEST_ARGC.store(argc as i32, Ordering::Relaxed);
    GUEST_ARGV.store(argv as usize as *mut *mut c_char, Ordering::Relaxed);
    // And the process's name, which the host's `warn` and `err` print.
    #[cfg(not(target_os = "macos"))]
    if argc > 0 && argv != 0 {
        // SAFETY: the vector just laid out on the guest's stack, whose first
        // entry is a NUL-terminated string in this address space.
        let argv0 = unsafe { *(argv as usize as *const *const c_char) };
        if !argv0.is_null() {
            darwin::progname::set(unsafe { std::ffi::CStr::from_ptr(argv0) });
        }
    }
}

/// `int *_NSGetArgc(void)`.
extern "C" fn ns_get_argc() -> *mut i32 {
    GUEST_ARGC.as_ptr()
}

/// `char ***_NSGetArgv(void)`.
extern "C" fn ns_get_argv() -> *mut *mut *mut c_char {
    GUEST_ARGV.as_ptr()
}

/// The guest's own executable, as `_NSGetExecutablePath` must report it.
///
/// Empty until [`set_executable_path`] runs, which is before any guest code.
static GUEST_EXECUTABLE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

/// Record the program the guest is, for the reason [`set_process_args`] records
/// its arguments: the path has to be the one the guest thinks it is running
/// from, not this runtime's.
///
/// Absolute, because a caller joins onto it, but with its symlinks kept, as
/// `Layout::enclosing` keeps them.
pub fn set_executable_path(path: &std::path::Path) {
    let resolved = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    // What the guest is told, so the *guest* spelling: a program outside the
    // prefix is named through the `/Volumes/Host` mount rather than by a host
    // path it would go on to compare and store. See `docs/prefixes.md`.
    let spelled = prefix::ns::to_guest(&resolved).unwrap_or(resolved);
    *GUEST_EXECUTABLE.lock().unwrap_or_else(|e| e.into_inner()) =
        spelled.to_string_lossy().into_owned();
}

/// The address of the *guest's* `environ` variable.
///
/// `getenv`/`setenv`/`putenv` work on a block of the guest's own - see
/// `darwin::env` for why - so a guest reading `environ`, or calling
/// `_NSGetEnviron` to get at it, which is how Darwin spells taking its address,
/// must be looking at that same block rather than at this process's.
///
/// Darwin does not export `environ` from a dylib at all, only the accessor;
/// every other host exports the variable and has no accessor. Both are
/// published, from one address.
#[cfg(not(target_os = "macos"))]
fn guest_environ() -> Option<u64> {
    Some(darwin::env::environ_slot() as usize as u64)
}

/// On macOS the guest runs against a real Darwin root and the host's own block
/// is the right one, exactly as before.
#[cfg(target_os = "macos")]
fn guest_environ() -> Option<u64> {
    if let Some(f) = crate::host::symbol("_NSGetEnviron") {
        // SAFETY: `_NSGetEnviron` is `char ***(void)`, a pure accessor for a
        // libSystem global, so its result is valid for the life of the process.
        let get: unsafe extern "C" fn() -> *mut *mut *mut c_char =
            unsafe { core::mem::transmute(f as usize as *const ()) };
        let addr = unsafe { get() };
        if !addr.is_null() {
            return Some(addr as usize as u64);
        }
    }
    crate::host::symbol("environ")
}

/// Whatever [`guest_environ`] found, resolved once at provider time.
static ENVIRON: AtomicPtr<*mut *mut c_char> = AtomicPtr::new(std::ptr::null_mut());

/// A stable address inside *this* binary, for glibc's `dso_symbol` argument.
///
/// It is not read: `__cxa_thread_atexit_impl` looks the address up to find the
/// object that owns the destructor and holds it in memory until the destructor
/// has run. The guest's images are never unloaded, so this runtime's own handle
/// says the true thing - the code will still be there.
static DSO_ANCHOR: u8 = 0;

/// `void _tlv_atexit(void (*dtor)(void *), void *obj)`, for a host without one.
///
/// Darwin registers a thread-local's destructor with dyld, which runs it when
/// the thread ends. glibc's equivalent is `__cxa_thread_atexit_impl`, whose only
/// difference is the third argument - see [`DSO_ANCHOR`].
///
/// A host with neither drops the destructor rather than faulting. That leaks
/// whatever the thread-local owned, which is a worse program than one whose
/// destructors run and a much better one than a null call in a constructor.
///
/// # Safety
///
/// Called through an import slot this module defines, with the two arguments the
/// Darwin declaration gives it. `dtor` is guest code and `obj` is the guest's
/// object; neither is touched here.
unsafe extern "C" fn tlv_atexit(dtor: Option<unsafe extern "C" fn(*mut c_void)>, obj: *mut c_void) {
    /// `__cxa_thread_atexit_impl`, looked up once.
    static HOST: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();

    let Some(dtor) = dtor else { return };
    let Some(f) =
        *HOST.get_or_init(|| crate::host::symbol("__cxa_thread_atexit_impl").map(|f| f as usize))
    else {
        return;
    };
    // SAFETY: glibc's `int __cxa_thread_atexit_impl(void (*)(void *), void *,
    // void *)`, the prototype named here.
    let register: unsafe extern "C" fn(
        unsafe extern "C" fn(*mut c_void),
        *mut c_void,
        *mut c_void,
    ) -> i32 = unsafe { core::mem::transmute(f as *const ()) };
    // SAFETY: `dtor` and `obj` are the caller's own, and the handle is this
    // binary's.
    unsafe {
        register(dtor, obj, &DSO_ANCHOR as *const u8 as *mut c_void);
    }
}

/// `char ***_NSGetEnviron(void)`.
extern "C" fn ns_get_environ() -> *mut *mut *mut c_char {
    ENVIRON.load(Ordering::Relaxed)
}

/// `int _NSGetExecutablePath(char *buf, uint32_t *bufsize)`.
///
/// # Safety
///
/// Called only through an import slot this module defines, with the two arguments the
/// declaration gives it.
unsafe extern "C" fn ns_get_executable_path(buf: *mut u8, bufsize: *mut u32) -> i32 {
    if bufsize.is_null() {
        return -1;
    }
    // The guest's, not this process's. Only a runtime that was handed no path -
    // which is no run that reaches a guest - falls back to its own.
    let path = GUEST_EXECUTABLE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let path = match path.is_empty() {
        false => path,
        true => std::env::current_exe()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    };
    let needed = path.len() + 1;
    // SAFETY: a guest `uint32_t *`, directly addressable under the identity mapping.
    let available = unsafe { *bufsize } as usize;
    if buf.is_null() || available < needed {
        unsafe { *bufsize = needed as u32 };
        return -1;
    }
    // SAFETY: `buf` has at least `needed` bytes, just checked against the caller's count.
    unsafe {
        std::ptr::copy_nonoverlapping(path.as_ptr(), buf, path.len());
        *buf.add(path.len()) = 0;
    }
    0
}

/// Symbols the runtime supplies itself, consulted before the host.
pub fn provider() -> SyntheticDylib {
    let mut lib = SyntheticDylib::new("<rosetta3 runtime>");
    lib.flat(true).define_data(
        "___stack_chk_guard",
        &STACK_CHK_GUARD as *const u64 as usize as u64,
    );
    // `bootstrap_thunk`, never `bootstrap`. The two are the same function
    // except for the calling convention dyld's `_tlv_get_addr` has and the
    // System V ABI does not: a `tlv_descriptor` thunk preserves *every*
    // register except the result, because a thread-local access is emitted
    // without spilling anything first. The plain `bootstrap` is an ordinary
    // C function and freely clobbers `rdi`..`r11` and `xmm0`..`xmm7`, so
    // publishing it loses a value the guest still needs - in Chromium
    // Framework, the `rsi` holding a moved-from object, and the next
    // instruction after the bootstrap is a load through it.
    //
    // This is the *only* provider in the `dlopen` chain that defines these
    // names, so a `dlopen`ed image's `__thread_vars` binds here, and the
    // program's own link reaches the same function through this provider too.
    for name in machish::extensions::tlv::BOOTSTRAP_SYMBOLS {
        lib.define_fn(name, crate::tls::bootstrap_thunk as *const ());
    }
    let atexit = crate::host::symbol("_tlv_atexit")
        .map_or(tlv_atexit as *const (), |f| f as usize as *const ());
    lib.define_fn("__tlv_atexit", atexit);
    lib.define_fn("_tlv_atexit", atexit);
    super::openat::provide(&mut lib);
    lib.define_fn("__NSGetExecutablePath", ns_get_executable_path as *const ());
    lib.define_fn(
        "__dyld_find_unwind_sections",
        crate::dyld::entry::dyld_find_unwind_sections as *const (),
    );
    if let Some(port) = crate::host::symbol("mach_task_self_") {
        lib.define_data("_mach_task_self_", port);
    }
    if let Some(f) = crate::host::symbol("mach_host_self") {
        lib.define_fn("_mach_host_self", f as *const ());
    }
    lib.define_fn("__NSGetArgc", ns_get_argc as *const ());
    lib.define_fn("__NSGetArgv", ns_get_argv as *const ());
    if let Some(addr) = guest_environ() {
        ENVIRON.store(addr as usize as *mut *mut *mut c_char, Ordering::Relaxed);
        lib.define_data("_environ", addr);
        lib.define_fn("__NSGetEnviron", ns_get_environ as *const ());
    }
    lib
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guard_has_a_zero_byte() {
        assert!(
            STACK_CHK_GUARD.to_le_bytes().contains(&0),
            "a canary without a zero byte can be copied back by a string overflow"
        );
    }

    #[test]
    fn the_executable_path_round_trips_and_reports_the_size_it_needs() {
        let expected = std::env::current_exe().unwrap();
        let expected = expected.to_string_lossy();

        // Too small a buffer must change nothing and report the size required,
        // including the NUL.
        let mut size: u32 = 1;
        // SAFETY: a null buffer with a size the function is being asked to grow.
        let rc = unsafe { ns_get_executable_path(std::ptr::null_mut(), &mut size) };
        assert_eq!(rc, -1);
        assert_eq!(size as usize, expected.len() + 1);

        let mut buf = vec![0xAAu8; size as usize + 8];
        // SAFETY: `buf` is at least `size` bytes, which is what was just asked for.
        let rc = unsafe { ns_get_executable_path(buf.as_mut_ptr(), &mut size) };
        assert_eq!(rc, 0);
        let end = buf.iter().position(|b| *b == 0).expect("NUL-terminated");
        assert_eq!(&buf[..end], expected.as_bytes());
        assert!(buf[end + 1..].iter().all(|b| *b == 0xAA));
    }
}
