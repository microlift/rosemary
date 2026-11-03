use std::ffi::{CStr, c_char, c_int, c_void};

extern "C" fn abort_report_np(_: *const c_char) -> ! {
    todo!()
}

unsafe extern "C" {
    #[cfg(target_env = "gnu")]
    fn __register_atfork(
        prepare: *const (),
        parent: *const (),
        child: *const (),
        dso: *const (),
    ) -> c_int;
}

#[cfg(target_env = "gnu")]
static DSO_HANDLE: u8 = 0;

pub unsafe extern "C" fn atexit(handler: *const ()) -> c_int {
    if handler.is_null() {
        return -1;
    }
    // SAFETY: a zero-argument handler ignores the register its argument arrives in.
    let func = unsafe { std::mem::transmute::<*const (), crate::exit::Handler>(handler) };
    crate::exit::register(func, core::ptr::null_mut(), 0, handler as u64)
}

/// `int __cxa_atexit(void (*func)(void *), void *arg, void *dso)`, which every
/// static destructor is registered with. Into [`crate::exit`]'s list, so it shares
/// one order with `atexit` and `dlclose` can finalize the image `dso` names.
pub unsafe extern "C" fn cxa_atexit(func: *const (), arg: *mut c_void, dso: *const ()) -> c_int {
    if func.is_null() {
        return -1;
    }
    // SAFETY: the caller states `func` takes `arg`.
    let handler = unsafe { std::mem::transmute::<*const (), crate::exit::Handler>(func) };
    crate::exit::register(handler, arg, dso as u64, func as u64)
}

pub extern "C" fn exit(status: c_int) -> ! {
    crate::exit::finalize(0);
    // SAFETY: ends the process; the remaining host handlers run as usual.
    unsafe { libc::exit(status) }
}

/// `void __cxa_finalize(void *dso)`
pub extern "C" fn cxa_finalize(dso: *const ()) {
    crate::exit::finalize(dso as u64);
}

const BLOCK_IS_GLOBAL: u32 = 1 << 28;

const BLOCK_FLAGS_OFFSET: u64 = 8;

const BLOCK_INVOKE_OFFSET: u64 = 16;

unsafe extern "C" fn run_exit_block(block: *mut c_void) {
    let block = block as u64;
    // SAFETY: a block this process copied or a global one, either way still live:
    // nothing releases it before exit.
    unsafe { crate::dispatch::block::invoke(block) };
    crate::dispatch::block::release(block);
}

pub unsafe extern "C" fn atexit_b(block: *mut c_void) -> c_int {
    if block.is_null() {
        return -1;
    }
    let kept = match crate::dispatch::block::copy(block as u64) {
        Some(copied) => copied,
        None => {
            // SAFETY: the caller states this is a block literal, whose `flags`
            // are at a fixed offset by the Block ABI.
            let flags = unsafe { ((block as u64 + BLOCK_FLAGS_OFFSET) as *const u32).read() };
            if flags & BLOCK_IS_GLOBAL == 0 {
                return -1;
            }
            block as u64
        }
    };
    // The block's code is what ties it to an image, not the wrapper that calls it.
    // SAFETY: a live block, whose `invoke` is at a fixed offset by the Block ABI.
    let invoke = unsafe { ((kept + BLOCK_INVOKE_OFFSET) as *const u64).read() };
    crate::exit::register(run_exit_block, kept as *mut c_void, 0, invoke)
}

#[cfg_attr(not(target_env = "gnu"), allow(unused_variables))]
pub unsafe extern "C" fn pthread_atfork(
    prepare: *const (),
    parent: *const (),
    child: *const (),
) -> c_int {
    // FIXME (ACCURACY): fork handlers are completely dropped on non-GNU systems.
    #[cfg(target_env = "gnu")]
    {
        // SAFETY: the caller's guarantee, and a DSO handle of this crate's own.
        unsafe { __register_atfork(prepare, parent, child, &raw const DSO_HANDLE as *const ()) }
    }
    #[cfg(not(target_env = "gnu"))]
    {
        return 0;
    }
}

pub unsafe extern "C" fn assert_rtn(
    function: *const c_char,
    file: *const c_char,
    line: c_int,
    expression: *const c_char,
) -> ! {
    // SAFETY: the caller's guarantee; a null is reported as such rather than read.
    let text = |p: *const c_char| unsafe {
        match p.is_null() {
            true => "<null>".to_string(),
            false => CStr::from_ptr(p).to_string_lossy().into_owned(),
        }
    };
    eprintln!(
        "Assertion failed: ({}), function {}, file {}, line {}.",
        text(expression),
        text(function),
        text(file),
        line
    );
    std::process::abort()
}

pub unsafe extern "C" fn srandomdev() {
    let mut seed = [0u8; 4];
    if getrandom::fill(&mut seed).is_err() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        seed = now.to_ne_bytes();
    }
    unsafe { libc::srand(u32::from_ne_bytes(seed)) };
}

pub unsafe extern "C" fn memset_s(
    dest: *mut c_void,
    destsz: usize,
    ch: c_int,
    count: usize,
) -> c_int {
    const RSIZE_MAX: usize = isize::MAX as usize;
    if dest.is_null() {
        return libc::EINVAL;
    }
    if destsz > RSIZE_MAX || count > RSIZE_MAX {
        return libc::ERANGE;
    }
    // On a violation the standard still requires the destination be cleared as
    // far as it goes, so the caller is never left holding the secret it asked to
    // destroy.
    let written = count.min(destsz);
    // SAFETY: `written` is within `destsz`, which the caller says is writable.
    // Volatile so the store survives an optimiser that can see the buffer dies.
    unsafe {
        let bytes = dest.cast::<u8>();
        for offset in 0..written {
            bytes.add(offset).write_volatile(ch as u8);
        }
    }
    match count > destsz {
        true => libc::ERANGE,
        false => 0,
    }
}

// FIXME (ACCURACY): STUB!
pub unsafe extern "C" fn sys_icache_invalidate(_start: *mut c_void, _len: usize) {}

/// A lock is deliberately isolated on its own cache line. Generic atomics are
/// often adjacent allocations; putting their independently selected locks in
/// one cache line would turn that into false sharing.
#[repr(align(64))]
struct AtomicLock(std::sync::Mutex<()>);

/// More buckets than compiler-rt's minimum, with a cache line per bucket, keep
/// unrelated large atomics from contending. A power of two makes the final
/// reduction a mask rather than a division.
static ATOMIC_LOCKS: [AtomicLock; 256] = [const { AtomicLock(std::sync::Mutex::new(())) }; 256];

fn atomic_lock_index(address: usize) -> usize {
    // Objects are at least word-aligned, so the low bits carry nothing. Fold
    // page-number bits down too: using only low bits maps every same-offset
    // object in different pages to one lock.
    let mut bits = address >> 3;
    bits ^= bits >> 16;
    bits ^= bits >> 8;
    bits & (ATOMIC_LOCKS.len() - 1)
}

fn atomic_lock(address: *const c_void) -> std::sync::MutexGuard<'static, ()> {
    ATOMIC_LOCKS[atomic_lock_index(address as usize)]
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

const ATOMIC_SEQ_CST: i32 = 5;

fn atomic_before(model: i32) {
    // Each address bucket has a separate lock. Their acquire/release operations
    // give all non-sequential orders the required synchronization, but cannot
    // make a single total order across buckets. SeqCst fences supply that order.
    if model == ATOMIC_SEQ_CST {
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }
}

fn atomic_after(model: i32) {
    if model == ATOMIC_SEQ_CST {
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// # Safety
///
/// `src` and `dest` must each be valid for `size` bytes.
pub unsafe extern "C" fn atomic_load(
    size: i32,
    src: *const c_void,
    dest: *mut c_void,
    _model: i32,
) {
    atomic_before(_model);
    let _guard = atomic_lock(src);
    unsafe { std::ptr::copy(src as *const u8, dest as *mut u8, size.max(0) as usize) };
    drop(_guard);
    atomic_after(_model);
}

/// # Safety
///
/// As [`atomic_load`].
pub unsafe extern "C" fn atomic_store(
    size: i32,
    dest: *mut c_void,
    src: *const c_void,
    _model: i32,
) {
    atomic_before(_model);
    let _guard = atomic_lock(dest);
    unsafe { std::ptr::copy(src as *const u8, dest as *mut u8, size.max(0) as usize) };
    drop(_guard);
    atomic_after(_model);
}

/// # Safety
///
/// `numstr` is NUL-terminated and `errstrp` is null or writable.
pub unsafe extern "C" fn strtonum(
    numstr: *const c_char,
    minval: i64,
    maxval: i64,
    errstrp: *mut *const c_char,
) -> i64 {
    let saved = crate::errno::get();
    crate::errno::set(0);
    let mut value = 0;
    let error: Option<(&'static CStr, c_int)> = if minval > maxval || numstr.is_null() {
        Some((c"invalid", libc::EINVAL))
    } else {
        let mut end: *mut c_char = std::ptr::null_mut();
        // SAFETY: the caller's string, and a local for the end pointer.
        value = unsafe { libc::strtoll(numstr, &mut end, 10) };
        let range = crate::errno::get() == libc::ERANGE;
        // SAFETY: `strtoll` leaves `end` inside the string.
        if end.cast_const() == numstr || unsafe { *end } != 0 {
            Some((c"invalid", libc::EINVAL))
        } else if (value == i64::MIN && range) || value < minval {
            Some((c"too small", libc::ERANGE))
        } else if (value == i64::MAX && range) || value > maxval {
            Some((c"too large", libc::ERANGE))
        } else {
            None
        }
    };
    let (text, errno) = match error {
        Some((text, errno)) => (text.as_ptr(), errno),
        None => (std::ptr::null(), saved),
    };
    if !errstrp.is_null() {
        unsafe { *errstrp = text };
    }
    crate::errno::set(errno);
    if error.is_some() { 0 } else { value }
}

#[cfg(target_os = "linux")]
unsafe extern "C" {
    /// glibc's and musl's real name for `sigsetjmp`, which their headers make a
    /// macro.
    fn __sigsetjmp(env: *mut c_void, savemask: c_int) -> c_int;
}

/// `int sigsetjmp(sigjmp_buf, int savemask)`, as a tail jump to the host's.
///
/// A jump rather than a call: `sigsetjmp` records its caller's frame, and a
/// wrapper frame would be gone by the time `siglongjmp` returns into it. The
/// host's own `siglongjmp` is the other half, and reads only what this wrote.
///
/// The buffers are compatible in size, which is the whole question: glibc
/// writes 80 bytes on x86-64 (registers, the saved-mask flag, and the 8-byte
/// kernel mask) into Darwin's 152, and 192 on arm64 into Darwin's 196.
///
/// FIXME (ACCURACY): I hate this. This will definitely cause unpredictable issues
/// on another OS or architecture. Please fix this. Please.
#[cfg(target_os = "linux")]
#[unsafe(naked)]
pub unsafe extern "C" fn sigsetjmp(_env: *mut c_void, _savemask: c_int) -> c_int {
    #[cfg(target_arch = "x86_64")]
    core::arch::naked_asm!("jmp {}", sym __sigsetjmp);
    #[cfg(target_arch = "aarch64")]
    core::arch::naked_asm!("b {}", sym __sigsetjmp);
}

const CS_OPS_STATUS: u32 = 0;

/// `int csops(pid_t, unsigned int ops, void *useraddr, size_t usersize)`
///
/// Only `CS_OPS_STATUS` of this process, answered with no flags: nothing here
/// is code-signed, and a Mac reports an unsigned process the same way. Apple's
/// bash asks to learn whether it runs under the installer's restrictions.
///
/// FIXME (ACCURACY): No real code signing, this always reports success!
///
/// # Safety
///
/// `useraddr` is writable for `usersize` bytes.
pub unsafe extern "C" fn csops(
    pid: libc::pid_t,
    ops: u32,
    useraddr: *mut c_void,
    usersize: usize,
) -> c_int {
    if pid != 0 && pid != crate::process::host_self() {
        crate::errno::set(libc::ESRCH);
        return -1;
    }
    if ops != CS_OPS_STATUS {
        crate::errno::set(libc::ENOTSUP);
        return -1;
    }
    // The kernel's own check: the flags word and nothing else.
    if useraddr.is_null() || usersize != size_of::<u32>() {
        crate::errno::set(libc::EINVAL);
        return -1;
    }
    // SAFETY: the caller's obligation, checked to be four bytes.
    unsafe { useraddr.cast::<u32>().write_unaligned(0) };
    0
}

/// `void _cthread_init(void)`
///
/// libSystem's start-up hook for the Mach cthreads package, which `pthread`
/// replaced. A guest that calls it wants the threading state to exist, and it
/// always does here.
pub extern "C" fn cthread_init() {}

/// `void (*_cthread_init_routine)(void)`
///
/// A data symbol: the Csu `crt1` start stub reads it and calls it when it is
/// non-null. No cthreads package is left to initialise, so it stays null.
static CTHREAD_INIT_ROUTINE: usize = 0;

/// `int (*mach_init_routine)(void)`
///
/// Another `crt1` hook read as data and called when non-null. The task's Mach
/// ports are made when the runtime starts, so it stays null.
static MACH_INIT_ROUTINE: usize = 0;

/// `void __keymgr_dwarf2_register_sections(void)`
///
/// libkeymgr's hand-off of an image's `__eh_frame` to the unwinder. Unwinding
/// here reads each mapped image's sections itself, so a guest asking for them to
/// be registered has nothing to wait for.
pub extern "C" fn keymgr_dwarf2_register_sections() {}

exports::exports! {
    functions {
        "abort_report_np" => abort_report_np,
        #[cfg(target_os = "linux")]
        "sigsetjmp" => sigsetjmp,
        "csops" => csops,
        "_cthread_init" => cthread_init,
        "__keymgr_dwarf2_register_sections" => keymgr_dwarf2_register_sections,
        "__atomic_load" => atomic_load,
        "__atomic_store" => atomic_store,
        "atexit" => atexit,
        "atexit_b" => atexit_b,
        "__cxa_atexit" => cxa_atexit,
        "__cxa_finalize" => cxa_finalize,
        "exit" => exit,
        "pthread_atfork" => pthread_atfork,
        "__assert_rtn" => assert_rtn,
        "srandomdev" => srandomdev,
        "memset_s" => memset_s,
        "strtonum" => strtonum,
        "sys_icache_invalidate" => sys_icache_invalidate,
    }
    // Data keys keep Mach-O's leading underscore, unlike function keys.
    data {
        "__cthread_init_routine" => &raw const CTHREAD_INIT_ROUTINE as usize as u64,
        "_mach_init_routine" => &raw const MACH_INIT_ROUTINE as usize as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host's `sigsetjmp` must fit in a buffer the size of Darwin's. Only
    /// the first return is exercised: Rust has no `returns_twice`, so a test
    /// that `siglongjmp`s back into its own frame would be undefined.
    #[cfg(target_os = "linux")]
    #[test]
    fn sigsetjmp_fits_a_darwin_sized_buffer() {
        #[cfg(target_arch = "x86_64")]
        const DARWIN: usize = 38 * 4;
        #[cfg(target_arch = "aarch64")]
        const DARWIN: usize = 49 * 4;
        #[repr(C, align(16))]
        struct Buf([u8; DARWIN + 64]);
        let mut buf = Buf([0xaa; DARWIN + 64]);
        // SAFETY: a live buffer; the environment is never jumped to.
        let rc = unsafe { sigsetjmp(buf.0.as_mut_ptr().cast(), 1) };
        assert_eq!(rc, 0);
        assert!(buf.0[..DARWIN].iter().any(|b| *b != 0xaa));
        assert!(buf.0[DARWIN..].iter().all(|b| *b == 0xaa));
    }

    #[test]
    fn crt1_init_hooks_resolve_as_null_data() {
        use machish::fixups::{LibraryRef, SymbolRef};
        use machish::resolve::{SymbolKind, SymbolProvider, SyntheticDylib};

        let mut lib = SyntheticDylib::new("<test>");
        provide(&mut lib);
        for name in ["__cthread_init_routine", "_mach_init_routine"] {
            let sym = lib
                .resolve(&SymbolRef {
                    name,
                    library: LibraryRef::Flat,
                    weak_import: false,
                })
                .unwrap_or_else(|| panic!("{name} resolves"));
            assert_eq!(sym.kind, SymbolKind::Data, "{name}");
            // SAFETY: the address of a live static word.
            assert_eq!(unsafe { *(sym.addr as *const usize) }, 0, "{name}");
        }
    }

    #[test]
    fn csops_reports_an_unsigned_process() {
        let mut flags = u32::MAX;
        // SAFETY: a live four-byte local.
        let rc = unsafe { csops(0, CS_OPS_STATUS, (&raw mut flags).cast(), 4) };
        assert_eq!((rc, flags), (0, 0));
        let rc = unsafe { csops(0, 99, (&raw mut flags).cast(), 4) };
        assert_eq!(rc, -1);
    }

    #[test]
    fn generic_atomics_copy_the_whole_value() {
        let stored = [7u8; 48];
        let mut cell = [0u8; 48];
        let mut loaded = [0u8; 48];
        // SAFETY: three buffers of the size passed.
        unsafe {
            atomic_store(48, cell.as_mut_ptr().cast(), stored.as_ptr().cast(), 5);
            atomic_load(48, cell.as_ptr().cast(), loaded.as_mut_ptr().cast(), 5);
        }
        assert_eq!(loaded, stored);
    }

    #[test]
    fn generic_atomic_locks_are_cache_line_isolated() {
        assert_eq!(std::mem::align_of::<AtomicLock>(), 64);
        assert!(std::mem::size_of::<AtomicLock>() >= 64);
    }

    #[test]
    fn generic_atomic_lock_hash_uses_page_number() {
        let indexes: std::collections::BTreeSet<_> = (1..=32)
            .map(|page| atomic_lock_index(page * 4096))
            .collect();
        assert_eq!(indexes.len(), 32, "same-offset pages must not share a lock");
    }

    /// The count is what is written, and a count past the destination is an
    /// error that still clears as much as it may.
    #[test]
    fn memset_s_clears_what_it_can_and_refuses_the_rest() {
        let mut buffer = [0xAAu8; 8];
        // SAFETY: eight writable bytes, and a count within them.
        let rc = unsafe { memset_s(buffer.as_mut_ptr().cast(), buffer.len(), 0, 4) };
        assert_eq!(rc, 0);
        assert_eq!(buffer, [0, 0, 0, 0, 0xAA, 0xAA, 0xAA, 0xAA]);

        // SAFETY: as above, with a count that overruns the size it was given.
        let rc = unsafe { memset_s(buffer.as_mut_ptr().cast(), buffer.len(), 0, 16) };
        assert_eq!(rc, libc::ERANGE);
        assert_eq!(buffer, [0u8; 8], "the destination is still cleared");
    }

    #[test]
    fn memset_s_refuses_a_null_destination_rather_than_writing_to_it() {
        // SAFETY: null with a zero size, which is what the check is for.
        assert_eq!(
            unsafe { memset_s(core::ptr::null_mut(), 0, 0, 0) },
            libc::EINVAL
        );
    }

    #[test]
    fn strtonum_bounds_and_explains_its_answer() {
        let mut why: *const c_char = std::ptr::null();
        // SAFETY: literal strings and a local for the reason.
        unsafe {
            assert_eq!(strtonum(c"42".as_ptr(), 0, 100, &mut why), 42);
            assert!(why.is_null());
            assert_eq!(strtonum(c"420".as_ptr(), 0, 100, &mut why), 0);
            assert_eq!(CStr::from_ptr(why), c"too large");
            assert_eq!(crate::errno::get(), libc::ERANGE);
            assert_eq!(strtonum(c"-1".as_ptr(), 0, 100, &mut why), 0);
            assert_eq!(CStr::from_ptr(why), c"too small");
            assert_eq!(strtonum(c"4x".as_ptr(), 0, 100, &mut why), 0);
            assert_eq!(CStr::from_ptr(why), c"invalid");
            assert_eq!(crate::errno::get(), libc::EINVAL);
        }
    }

    #[test]
    fn a_test_process_is_not_setuid() {
        assert_eq!(crate::credentials::issetugid(), 0);
    }

    /// Seeding twice from the kernel gives two different sequences, which is the
    /// difference between this and `srandom(1)`.
    #[test]
    fn srandomdev_seeds_from_something_that_changes() {
        // SAFETY: takes and touches nothing.
        unsafe { srandomdev() };
        let first = unsafe { libc::rand() };
        unsafe { srandomdev() };
        let second = unsafe { libc::rand() };
        assert_ne!(first, second, "the seed did not change between calls");
    }
}
