pub mod attr;
mod cond;
pub mod consts;
pub mod handle;
mod mutex;
pub mod object;
mod once;
pub mod raw;
mod rwlock;
mod sched;
pub mod sync;

#[cfg(test)]
pub(crate) mod testing;

pub use attr::{Attributes, HostAttr};
pub use consts::*;
pub use sync::{MutexAttr, once};

use object::{Kind, Record};

/// An opaque per-thread key, unique for as long as the thread lives.
///
/// Used only to find a thread's state from another thread. Darwin's answer is its Mach
/// port, which is what `pthread_mach_thread_np` returns and what a Mach exception
/// message carries; elsewhere it is the kernel thread id, which has the same two
/// properties that matter here - unique while the thread runs, and readable from the
/// thread itself without a lock.
///
/// A `u32` because both underlying values are one - a `mach_port_t` and a `pid_t` -
/// and because the registry that keys on it compares it atomically.
///
/// Not comparable across hosts and not meaningful to a guest. A guest asking for its
/// own Mach port is a different question, answered by `rosemary::shims::machthread`.
pub fn id() -> u32 {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: takes this thread's own `pthread_t`, which is live by definition.
        unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) }
    }
    #[cfg(target_os = "linux")]
    {
        let _host = crate::syscall::ScopedHostSyscallContext::new();
        rustix::thread::gettid().as_raw_pid() as u32
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        unsafe { libc::pthread_self() as u32 }
    }
}

/// A `pthread_t` that names no thread.
///
/// `pthread_t` is an integer on Darwin and glibc and a pointer under musl, so the
/// out-parameter `pthread_create` fills cannot be initialised with a literal `0`.
pub fn null_thread() -> libc::pthread_t {
    // SAFETY: `pthread_t` is a scalar - an integer or a pointer - on every platform
    // this builds for, and all-zero is a valid bit pattern for either. The value is
    // only ever written over by `pthread_create` before being read.
    unsafe { core::mem::zeroed() }
}

/// A guest `pthread_t` as this host's. See [`handle`] for why they differ.
pub fn thread_of(guest: u64) -> libc::pthread_t {
    let host = handle::host_of(guest);
    // SAFETY: `pthread_t` is pointer-sized on both platforms, and `host` is a value
    // the host's own `pthread_create` produced.
    unsafe { core::mem::transmute_copy(&host) }
}

fn thread_bits(thread: libc::pthread_t) -> u64 {
    // SAFETY: `pthread_t` is pointer-sized on supported LP64 hosts.
    unsafe { core::mem::transmute_copy(&thread) }
}

mod thread_ids {
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    const CAPACITY: usize = 1024;
    static THREADS: [AtomicU64; CAPACITY] = [const { AtomicU64::new(0) }; CAPACITY];
    static IDS: [AtomicU32; CAPACITY] = [const { AtomicU32::new(0) }; CAPACITY];
    static QOS: [AtomicU64; CAPACITY] = [const { AtomicU64::new(0) }; CAPACITY];

    pub fn register(thread: u64, id: u32, qos: u64) {
        let start = thread as usize % CAPACITY;
        for offset in 0..CAPACITY {
            let index = (start + offset) % CAPACITY;
            let seen = THREADS[index].load(Ordering::Acquire);
            if seen == thread {
                QOS[index].store(qos, Ordering::Relaxed);
                IDS[index].store(id, Ordering::Release);
                return;
            }
            if seen == 0
                && THREADS[index]
                    .compare_exchange(0, thread, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                QOS[index].store(qos, Ordering::Relaxed);
                IDS[index].store(id, Ordering::Release);
                return;
            }
        }
    }

    pub fn set_qos(thread: u64, qos: u64) {
        let start = thread as usize % CAPACITY;
        for offset in 0..CAPACITY {
            let index = (start + offset) % CAPACITY;
            match THREADS[index].load(Ordering::Acquire) {
                seen if seen == thread => {
                    QOS[index].store(qos, Ordering::Release);
                    return;
                }
                0 => return,
                _ => {}
            }
        }
    }

    pub fn qos(thread: u64) -> Option<u64> {
        let start = thread as usize % CAPACITY;
        for offset in 0..CAPACITY {
            let index = (start + offset) % CAPACITY;
            match THREADS[index].load(Ordering::Acquire) {
                seen if seen == thread => return Some(QOS[index].load(Ordering::Acquire)),
                0 => return None,
                _ => {}
            }
        }
        None
    }

    pub fn get(thread: u64) -> Option<u32> {
        let start = thread as usize % CAPACITY;
        for offset in 0..CAPACITY {
            let index = (start + offset) % CAPACITY;
            match THREADS[index].load(Ordering::Acquire) {
                seen if seen == thread => {
                    let id = IDS[index].load(Ordering::Acquire);
                    return (id != 0).then_some(id);
                }
                0 => return None,
                _ => {}
            }
        }
        None
    }
}

/// Publish this thread's Darwin id under its own `pthread_t`.
///
/// [`create`] does this from inside [`run`], but a runtime that starts guest
/// threads itself - the emulated backend does, because a guest thread needs a
/// guest stack as well as a host one - never reaches [`create`]. Without it
/// [`mach_thread_np`] answers zero for every thread but the caller's own, and a
/// guest that records a thread's id at creation and compares it later - which is
/// what `IsInRenderingThread()` is - compares against zero forever.
///
/// Called from the new thread, before anything guest-visible runs on it.
pub fn publish_self() {
    // SAFETY: this thread is live by definition.
    thread_ids::register(thread_bits(unsafe { libc::pthread_self() }), id(), 0);
    handle::begin();
}

/// Build the `init`/`destroy`/`setter`/`getter` quartet for an attribute object whose
/// whole state is one packed word.
macro_rules! simple_attr {
    ($kind:expr, $init:ident, $destroy:ident, $default:expr) => {
        /// # Safety
        ///
        /// Called only through an import slot this crate defines; the argument is the
        /// guest's own attribute object.
        pub unsafe extern "C" fn $init(at: u64) -> i32 {
            if at == 0 {
                return err::EINVAL;
            }
            let r = unsafe { Record::at(at, $kind) };
            object::publish_cell(&r, $default);
            0
        }

        /// # Safety
        ///
        /// As above. Frees nothing: an attribute object owns no allocation.
        pub unsafe extern "C" fn $destroy(at: u64) -> i32 {
            if at == 0 {
                return err::EINVAL;
            }
            let r = unsafe { Record::at(at, $kind) };
            object::destroy_plain(&r)
        }
    };
}

simple_attr!(
    Kind::MutexAttr,
    mutexattr_init,
    mutexattr_destroy,
    MutexAttr::default().packed_public()
);
simple_attr!(
    Kind::CondAttr,
    condattr_init,
    condattr_destroy,
    PTHREAD_PROCESS_PRIVATE as u64
);
simple_attr!(
    Kind::RwLockAttr,
    rwlockattr_init,
    rwlockattr_destroy,
    PTHREAD_PROCESS_PRIVATE as u64
);

/// The packed word behind a `pthread_mutexattr_t`, read and written field by field.
///
/// # Safety
///
/// `at` is a guest `pthread_mutexattr_t *`.
unsafe fn mutexattr_word(at: u64) -> Option<Record> {
    if at == 0 {
        return None;
    }
    let r = unsafe { Record::at(at, Kind::MutexAttr) };
    r.is_ours().then_some(r)
}

/// `int pthread_mutexattr_settype(pthread_mutexattr_t *, int)`.
///
/// The one setter whose value the guest and the host disagree about, and the reason
/// this file is not a passthrough.
///
/// # Safety
///
/// As [`mutexattr_init`].
pub unsafe extern "C" fn mutexattr_settype(at: u64, kind: u64) -> i32 {
    let Some(r) = (unsafe { mutexattr_word(at) }) else {
        return err::EINVAL;
    };
    let kind = kind as i32;
    if !matches!(
        kind,
        PTHREAD_MUTEX_NORMAL | PTHREAD_MUTEX_ERRORCHECK | PTHREAD_MUTEX_RECURSIVE
    ) {
        return err::EINVAL;
    }
    let mut settings =
        MutexAttr::unpacked_public(r.cell().load(std::sync::atomic::Ordering::Relaxed));
    settings.kind = kind;
    r.cell().store(
        settings.packed_public(),
        std::sync::atomic::Ordering::Relaxed,
    );
    0
}

/// # Safety
///
/// As [`mutexattr_init`], with `out` a writable `int *`.
pub unsafe extern "C" fn mutexattr_gettype(at: u64, out: u64) -> i32 {
    let Some(r) = (unsafe { mutexattr_word(at) }) else {
        return err::EINVAL;
    };
    if out == 0 {
        return err::EINVAL;
    }
    let settings = MutexAttr::unpacked_public(r.cell().load(std::sync::atomic::Ordering::Relaxed));
    // SAFETY: the caller states `out` is a writable `int *`.
    unsafe { *(out as usize as *mut i32) = settings.kind };
    0
}

pub unsafe extern "C" fn mutexattr_setprotocol(at: u64, protocol: u64) -> i32 {
    let Some(r) = (unsafe { mutexattr_word(at) }) else {
        return err::EINVAL;
    };
    let protocol = protocol as i32;
    if !matches!(
        protocol,
        PTHREAD_PRIO_NONE | PTHREAD_PRIO_INHERIT | PTHREAD_PRIO_PROTECT
    ) {
        return err::EINVAL;
    }
    let mut settings =
        MutexAttr::unpacked_public(r.cell().load(std::sync::atomic::Ordering::Relaxed));
    settings.protocol = protocol;
    r.cell().store(
        settings.packed_public(),
        std::sync::atomic::Ordering::Relaxed,
    );
    0
}

pub unsafe extern "C" fn mutexattr_getprotocol(at: u64, out: u64) -> i32 {
    let Some(r) = (unsafe { mutexattr_word(at) }) else {
        return err::EINVAL;
    };
    if out == 0 {
        return err::EINVAL;
    }
    let settings = MutexAttr::unpacked_public(r.cell().load(std::sync::atomic::Ordering::Relaxed));
    unsafe { *(out as usize as *mut i32) = settings.protocol };
    0
}

/// `int pthread_mutexattr_setpolicy_np(pthread_mutexattr_t *, int)`.
///
/// The policy picks which waiter Darwin hands the lock to; the host has no equivalent,
/// and neither policy changes what a correct program observes, so the value is checked
/// and discarded. It cannot be dropped instead: the symbol does not exist on this host,
/// so an unbound import is refused by name and the guest never starts.
///
/// # Safety
///
/// As [`mutexattr_init`].
pub unsafe extern "C" fn mutexattr_setpolicy_np(at: u64, policy: u64) -> i32 {
    if unsafe { mutexattr_word(at) }.is_none() {
        return err::EINVAL;
    }
    match policy as i32 {
        PTHREAD_MUTEX_POLICY_FAIRSHARE_NP | PTHREAD_MUTEX_POLICY_FIRSTFIT_NP => 0,
        _ => err::EINVAL,
    }
}

/// The `pshared` setter, shared by all three attribute kinds.
macro_rules! pshared {
    ($setter:ident, $getter:ident, $kind:expr, $field:ident) => {
        /// # Safety
        ///
        /// As [`mutexattr_init`].
        pub unsafe extern "C" fn $setter(at: u64, shared: u64) -> i32 {
            let shared = shared as i32;
            if shared != PTHREAD_PROCESS_SHARED && shared != PTHREAD_PROCESS_PRIVATE {
                return err::EINVAL;
            }
            if at == 0 {
                return err::EINVAL;
            }
            let r = unsafe { Record::at(at, $kind) };
            if !r.is_ours() {
                return err::EINVAL;
            }
            $field(&r, shared);
            0
        }

        /// # Safety
        ///
        /// As [`mutexattr_init`], with `out` a writable `int *`.
        pub unsafe extern "C" fn $getter(at: u64, out: u64) -> i32 {
            if at == 0 || out == 0 {
                return err::EINVAL;
            }
            let r = unsafe { Record::at(at, $kind) };
            if !r.is_ours() {
                return err::EINVAL;
            }
            // SAFETY: the caller states `out` is a writable `int *`.
            unsafe { *(out as usize as *mut i32) = read_pshared(&r, $kind) };
            0
        }
    };
}

fn store_mutex_pshared(r: &Record, shared: i32) {
    let mut settings =
        MutexAttr::unpacked_public(r.cell().load(std::sync::atomic::Ordering::Relaxed));
    settings.pshared = shared;
    r.cell().store(
        settings.packed_public(),
        std::sync::atomic::Ordering::Relaxed,
    );
}

fn store_simple_pshared(r: &Record, shared: i32) {
    r.cell()
        .store(shared as u64, std::sync::atomic::Ordering::Relaxed);
}

fn read_pshared(r: &Record, kind: Kind) -> i32 {
    let word = r.cell().load(std::sync::atomic::Ordering::Relaxed);
    match kind {
        Kind::MutexAttr => MutexAttr::unpacked_public(word).pshared,
        _ => word as i32,
    }
}

pshared!(
    mutexattr_setpshared,
    mutexattr_getpshared,
    Kind::MutexAttr,
    store_mutex_pshared
);
pshared!(
    condattr_setpshared,
    condattr_getpshared,
    Kind::CondAttr,
    store_simple_pshared
);
pshared!(
    rwlockattr_setpshared,
    rwlockattr_getpshared,
    Kind::RwLockAttr,
    store_simple_pshared
);

/// `int pthread_kill(pthread_t, int)`.
///
/// The signal number is Darwin's and has to be translated: `SIGUSR1` is 30 there and
/// 10 here, so an untranslated call sends `SIGPWR` to a thread expecting a user
/// signal. See [`crate::signal::signal_to_host`].
///
/// # Safety
///
/// Called only through an import slot this crate defines.
pub unsafe extern "C" fn kill(thread: u64, signal: u64) -> i32 {
    // Signal zero is the "does this thread exist" probe, and passes through as itself.
    let host = match signal {
        0 => 0,
        s => match crate::signal::signal_to_host(s as i32) {
            Some(host) => host,
            None => return err::EINVAL,
        },
    };
    // SAFETY: `thread` is a handle the host's own `pthread_create` produced.
    err::from_host(unsafe { libc::pthread_kill(thread_of(thread), host) })
}

/// FIXME (ACCURACY): are we missing cancellation points anywhere?
/// are host cancellation points accurate to Darwin?
pub unsafe extern "C" fn cancel(thread: u64) -> i32 {
    err::from_host(unsafe { libc::pthread_cancel(thread_of(thread)) })
}

/// `void *pthread_get_stackaddr_np(pthread_t)` - the **highest** address of the
/// thread's stack, which is where a stack that grows down begins.
///
/// Darwin's answer is the top; glibc's `pthread_getattr_np` reports the *low*
/// address and the size, so the two have to be added. A guest handed the low
/// address instead gets a stack it believes lies above its real one, which is
/// how a crash reporter comes to walk unmapped memory.
///
/// Zero when the host will not say, which is what Darwin answers for a thread
/// it does not know.
///
/// # Safety
///
/// `thread` is a `pthread_t` this process issued.
pub unsafe extern "C" fn get_stackaddr_np(thread: u64) -> u64 {
    if let Some(stack) = moved_main_stack(thread) {
        return stack.end;
    }
    #[cfg(target_os = "macos")]
    // SAFETY: a handle this process issued.
    return unsafe { libc::pthread_get_stackaddr_np(thread_of(thread)) } as u64;
    #[cfg(not(target_os = "macos"))]
    match unsafe { stack_of(thread) } {
        Some((low, size)) => low.wrapping_add(size as u64),
        None => 0,
    }
}

/// `size_t pthread_get_stacksize_np(pthread_t)`
///
/// # Safety
///
/// As [`get_stackaddr_np`].
pub unsafe extern "C" fn get_stacksize_np(thread: u64) -> u64 {
    if let Some(stack) = moved_main_stack(thread) {
        return stack.end - stack.start;
    }
    #[cfg(target_os = "macos")]
    // SAFETY: a handle this process issued.
    return unsafe { libc::pthread_get_stacksize_np(thread_of(thread)) } as u64;
    #[cfg(not(target_os = "macos"))]
    unsafe { stack_of(thread) }.map_or(0, |(_, size)| size as u64)
}

/// The main thread's `pthread_t` and the stack the runtime moved it onto.
static MAIN_STACK: std::sync::OnceLock<(u64, core::ops::Range<u64>)> = std::sync::OnceLock::new();

/// Record that the calling thread - the process's main one - is about to run on
/// `stack` instead of the stack the host started it on.
///
/// An `LC_UNIXTHREAD` program is entered on a stack the runtime laid its
/// arguments out on, and the host's attributes still describe the old one. A
/// conservative collector (Mono's Boehm) scans from its stack pointer up to
/// what `pthread_get_stackaddr_np` says is the top, so the old answer walks it
/// off the end of the new stack.
pub fn set_main_stack(stack: core::ops::Range<u64>) {
    // SAFETY: always safe to call.
    let me = thread_bits(unsafe { libc::pthread_self() });
    let _ = MAIN_STACK.set((me, stack));
}

/// Map all of the calling (main) thread's stack, as Darwin's exec does, and
/// report that stack from then on.
///
/// Darwin's main stack is `RLIMIT_STACK` bytes below its top, all mapped. A
/// Linux one is mapped only as it grows, and glibc reports it a little smaller
/// than the limit. HotSpot guards the bottom pages of the thread it starts on,
/// placed by the limit, and `mprotect` on the unmapped range fails.
pub fn commit_main_stack() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: a zeroed attribute object is what `pthread_getattr_np` fills
        // in, and it is only read on success.
        let mut attr: libc::pthread_attr_t = unsafe { core::mem::zeroed() };
        // SAFETY: the calling thread and a live attribute object.
        if unsafe { libc::pthread_getattr_np(libc::pthread_self(), &mut attr) } != 0 {
            return;
        }
        let mut low = core::ptr::null_mut();
        let mut size = 0;
        // SAFETY: an attribute object `pthread_getattr_np` initialised.
        unsafe {
            libc::pthread_attr_getstack(&attr, &mut low, &mut size);
            libc::pthread_attr_destroy(&mut attr);
        }
        let top = low as u64 + size as u64;
        let mut limit: libc::rlimit = unsafe { core::mem::zeroed() };
        // SAFETY: a live out-parameter.
        let limited = unsafe { libc::getrlimit(libc::RLIMIT_STACK, &mut limit) } == 0
            && limit.rlim_cur != libc::RLIM_INFINITY;
        let bottom = match limited {
            true => top.saturating_sub(limit.rlim_cur),
            false => low as u64,
        };
        let here = &raw const size as u64;
        let Some(grown) = starnix::process::regions().ok().and_then(|regions| {
            regions
                .into_iter()
                .find(|region| (region.start..region.end).contains(&here))
        }) else {
            return;
        };
        if bottom == 0 || grown.start <= bottom {
            return;
        }
        // SAFETY: NOREPLACE fails rather than take over anything in the range.
        let mapped = unsafe {
            libc::mmap(
                bottom as *mut core::ffi::c_void,
                (grown.start - bottom) as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE
                    | libc::MAP_ANONYMOUS
                    | libc::MAP_FIXED_NOREPLACE
                    | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if mapped as u64 == bottom {
            set_main_stack(bottom..top);
        }
    }
}

/// The moved stack, when `thread` is the main thread and it was moved.
fn moved_main_stack(thread: u64) -> Option<core::ops::Range<u64>> {
    let (main, stack) = MAIN_STACK.get()?;
    (thread_bits(thread_of(thread)) == *main).then(|| stack.clone())
}

/// The `(lowest address, size)` of `thread`'s stack, out of the host's
/// attributes.
///
/// # Safety
///
/// As [`get_stackaddr_np`].
#[cfg(not(target_os = "macos"))]
unsafe fn stack_of(thread: u64) -> Option<(u64, usize)> {
    // SAFETY: zeroed is not a valid `pthread_attr_t` to *read*, but it is what
    // `pthread_getattr_np` is given to fill in, and it is only read on success.
    let mut attr: libc::pthread_attr_t = unsafe { core::mem::zeroed() };
    // FreeBSD spells this `pthread_attr_get_np`, and unlike glibc's it fills in
    // an attribute object the caller has already initialised.
    #[cfg(target_os = "freebsd")]
    // SAFETY: a live local, then a handle this process issued.
    let rc = unsafe {
        match libc::pthread_attr_init(&mut attr) {
            0 => libc::pthread_attr_get_np(thread_of(thread), &mut attr),
            rc => rc,
        }
    };
    #[cfg(not(target_os = "freebsd"))]
    // SAFETY: a handle this process issued and a live attribute object.
    let rc = unsafe { libc::pthread_getattr_np(thread_of(thread), &mut attr) };
    if rc != 0 {
        return None;
    }
    let mut low: *mut core::ffi::c_void = core::ptr::null_mut();
    let mut size: usize = 0;
    // SAFETY: an attribute object just filled in, and two live locals.
    let rc = unsafe { libc::pthread_attr_getstack(&attr, &mut low, &mut size) };
    // SAFETY: the same object, which is this function's to destroy.
    unsafe { libc::pthread_attr_destroy(&mut attr) };
    (rc == 0).then_some((low as usize as u64, size))
}

/// `int pthread_setname_np(const char *)` - **one** argument on Darwin, where Linux
/// takes the thread as well.
///
/// A guest calling the host's directly would pass its name where the thread belongs.
///
/// # Safety
///
/// `name` is the guest's own NUL-terminated string.
pub unsafe extern "C" fn setname_np(name: u64) -> i32 {
    if name == 0 {
        return err::EINVAL;
    }
    // SAFETY: the caller's NUL-terminated name.
    let full = unsafe { core::ffi::CStr::from_ptr(name as usize as *const libc::c_char) };
    crate::process::name_thread(&full.to_string_lossy());
    #[cfg(target_os = "macos")]
    {
        // SAFETY: the caller's guarantee; Darwin names the calling thread.
        err::from_host(unsafe { libc::pthread_setname_np(name as usize as *const libc::c_char) })
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux caps a thread name at sixteen bytes including the NUL and fails the
        // whole call with `ERANGE` for a longer one, where Darwin truncates. Truncated
        // here so a guest with a descriptive name gets a name rather than an error.
        // SAFETY: the caller states `name` is NUL-terminated.
        let mut buf = [0u8; 16];
        let bytes = full.to_bytes();
        let n = bytes.len().min(buf.len() - 1);
        buf[..n].copy_from_slice(&bytes[..n]);
        // SAFETY: `buf` is NUL-terminated by construction and outlives the call.
        err::from_host(unsafe {
            libc::pthread_setname_np(libc::pthread_self(), buf.as_ptr() as *const libc::c_char)
        })
    }
}

/// `int pthread_getname_np(pthread_t, char *, size_t)`.
pub unsafe extern "C" fn getname_np(thread: u64, name: u64, len: u64) -> i32 {
    if name == 0 || len == 0 {
        return err::EINVAL;
    }
    err::from_host(unsafe {
        libc::pthread_getname_np(
            thread_of(thread),
            name as usize as *mut libc::c_char,
            len as usize,
        )
    })
}

/// `void pthread_jit_write_protect_np(int enabled)`
///
/// Apple Silicon's per-thread switch between writing and executing `MAP_JIT`
/// memory. Off a Mac nothing enforces it - `map_flags` drops `MAP_JIT` - so
/// there is nothing to toggle. On one the guest's own `MAP_JIT` mappings are
/// the host's, and the toggle has to reach it or the guest's code writes fault.
pub extern "C" fn jit_write_protect_np(enabled: i32) {
    #[cfg(target_os = "macos")]
    // SAFETY: a thread-local toggle with no memory effects of its own.
    unsafe {
        libc::pthread_jit_write_protect_np(enabled)
    };
    #[cfg(not(target_os = "macos"))]
    let _ = enabled;
}

/// Non-zero on the thread that ran `main`. Answered by comparing thread ids rather
/// than by asking libpthread, which has no such call here.
pub unsafe extern "C" fn main_np() -> i32 {
    use static_init::dynamic;

    // static_init::dynamic is a necessity here because it runs on the main thread always as a constructor
    #[dynamic]
    static MAIN: u32 = id();

    i32::from(*MAIN == id())
}

pub unsafe extern "C" fn cpu_number_np(out: u64) -> i32 {
    if out == 0 {
        return err::EINVAL;
    }
    #[cfg(target_os = "linux")]
    let cpu = unsafe { libc::sched_getcpu() }.max(0) as u64;
    #[cfg(not(target_os = "linux"))]
    let cpu = 0u64;
    unsafe { *(out as usize as *mut u64) = cpu };
    0
}

/// A null thread means the calling one. macOS supplies its kernel thread id;
/// other hosts use the id published with the thread's handle.
///
/// # Safety
///
/// `out` is null or a writable `uint64_t *`.
pub unsafe extern "C" fn threadid_np(thread: u64, out: u64) -> i32 {
    if out == 0 {
        return err::EINVAL;
    }
    #[cfg(target_os = "macos")]
    {
        let host = if thread == 0 {
            null_thread()
        } else {
            thread_of(thread)
        };
        return unsafe { libc::pthread_threadid_np(host, out as usize as *mut u64) };
    }
    #[cfg(not(target_os = "macos"))]
    {
        if thread != 0 {
            // SAFETY: `thread` is a live handle; `pthread_equal` only compares.
            if unsafe { libc::pthread_equal(thread_of(thread), libc::pthread_self()) } == 0 {
                let Some(id) = thread_ids::get(handle::host_of(thread)) else {
                    return err::ESRCH;
                };
                unsafe { *(out as usize as *mut u64) = u64::from(id) };
                return 0;
            }
        }
        // SAFETY: the caller states `out` is a writable `uint64_t *`.
        unsafe { *(out as usize as *mut u64) = u64::from(id()) };
        0
    }
}

/// `mach_port_t pthread_mach_thread_np(pthread_t)`, Darwin's own.
///
/// A borrowed builtin Mach thread name on macOS, or a per-thread token elsewhere.
/// The vendored `libc++abi`
/// keys `__cxa_guard_acquire`'s recursion check on it, which is why a program
/// with a static initializer in a C++ image needs this on Linux at all - so
/// [`id`] is the answer, unique while the thread runs.
///
pub unsafe extern "C" fn mach_thread_np(thread: u64) -> u32 {
    #[cfg(target_os = "macos")]
    {
        let mut tid = 0u64;
        if unsafe { threadid_np(thread, (&raw mut tid) as usize as u64) } != 0 {
            return 0;
        }
        return crate::mach::space::borrow_thread(crate::process::host_self(), tid as i32);
    }
    #[cfg(not(target_os = "macos"))]
    {
        if thread != 0 {
            // SAFETY: `thread` is a live handle; `pthread_equal` only compares.
            if unsafe { libc::pthread_equal(thread_of(thread), libc::pthread_self()) } == 0 {
                return thread_ids::get(handle::host_of(thread)).unwrap_or(0);
            }
        }
        id()
    }
}

/// `int pthread_yield_np(void)`, Darwin's spelling of `sched_yield`.
pub unsafe extern "C" fn yield_np() -> i32 {
    unsafe { libc::sched_yield() }
}

/// `int pthread_set_qos_class_self_np(qos_class_t, int)`.
///
/// A QoS class is one name for what Linux splits across a nice value, an I/O class and
/// a timer slack, so only the part a host can honour without privilege is applied: the
/// classes at or below the default become a nice value, and the two above it are
/// accepted and left alone, since raising a thread's priority needs `CAP_SYS_NICE` and
/// a guest that asks to run *faster* must not be failed for it.
///
/// Darwin's validation is kept, so a guest computing a class or a relative priority
/// wrongly still hears about it - and unlike the rest of the `pthread_` family this
/// call reports that the libc way, returning -1 with `errno` set rather than the error
/// itself. That is what libpthread does, and a caller reading `errno` after a `-1`
/// would otherwise see whatever was there before.
pub unsafe extern "C" fn set_qos_class_self_np(class: u32, relative: i32) -> i32 {
    // `None` for the two classes above the default: recognised, so not an error, but
    // nothing a host can apply.
    let nice = match class {
        QOS_CLASS_BACKGROUND => Some(10),
        QOS_CLASS_UTILITY => Some(5),
        QOS_CLASS_DEFAULT | QOS_CLASS_UNSPECIFIED => Some(0),
        QOS_CLASS_USER_INITIATED | QOS_CLASS_USER_INTERACTIVE => None,
        _ => return qos_einval(),
    };
    if !(QOS_MIN_RELATIVE_PRIORITY..=0).contains(&relative) {
        return qos_einval();
    }
    #[cfg(target_os = "linux")]
    if let Some(nice) = nice {
        // A relative priority runs 0 down to -15 within a class, so it lowers this
        // thread a little further inside the band the class already chose.
        //
        // SAFETY: `PRIO_PROCESS` with a `who` of zero names the calling thread on
        // Linux, so this touches no other thread and no memory. A failure here is a
        // scheduling hint not taken, which is not the guest's error to see.
        unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice - relative / 4) };
    }
    #[cfg(not(target_os = "linux"))]
    let _ = nice;
    let packed = u64::from(class) | ((relative as u32 as u64) << 32);
    CURRENT_QOS.with(|qos| qos.set(packed));
    thread_ids::set_qos(thread_bits(unsafe { libc::pthread_self() }), packed);
    0
}

thread_local! {
    static CURRENT_QOS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn valid_qos(class: u32, relative: i32) -> bool {
    matches!(
        class,
        QOS_CLASS_BACKGROUND
            | QOS_CLASS_UTILITY
            | QOS_CLASS_DEFAULT
            | QOS_CLASS_UNSPECIFIED
            | QOS_CLASS_USER_INITIATED
            | QOS_CLASS_USER_INTERACTIVE
    ) && (QOS_MIN_RELATIVE_PRIORITY..=0).contains(&relative)
}

pub unsafe extern "C" fn get_qos_class_np(thread: u64, class: u64, relative: u64) -> i32 {
    if class == 0 || relative == 0 {
        return err::EINVAL;
    }
    let packed = if thread == 0
        || unsafe { libc::pthread_equal(thread_of(thread), libc::pthread_self()) } != 0
    {
        CURRENT_QOS.with(std::cell::Cell::get)
    } else {
        let Some(qos) = thread_ids::qos(handle::host_of(thread)) else {
            return err::ESRCH;
        };
        qos
    };
    unsafe {
        *(class as usize as *mut u32) = packed as u32;
        *(relative as usize as *mut i32) = (packed >> 32) as i32;
    }
    0
}

/// `pthread_set_qos_class_self_np`'s failure: the libc convention, not the pthread one.
fn qos_einval() -> i32 {
    crate::errno::set(libc::EINVAL);
    -1
}

/// `int pthread_setcancelstate(int, int *)`, whose two states are inverted.
///
/// Darwin's `PTHREAD_CANCEL_ENABLE` is 1 and Linux's is 0, so an untranslated call
/// disables cancellation where the guest asked to enable it.
///
/// # Safety
///
/// `old` is null or a writable `int *`.
pub unsafe extern "C" fn setcancelstate(state: u64, old: u64) -> i32 {
    let host = match state as i32 {
        PTHREAD_CANCEL_ENABLE => host_cancel_enable(),
        PTHREAD_CANCEL_DISABLE => host_cancel_disable(),
        _ => return err::EINVAL,
    };
    let mut previous = 0;
    // SAFETY: a live local.
    let rc = unsafe { raw::pthread_setcancelstate(host, &mut previous) };
    if rc == 0 && old != 0 {
        let darwin = match previous == host_cancel_enable() {
            true => PTHREAD_CANCEL_ENABLE,
            false => PTHREAD_CANCEL_DISABLE,
        };
        // SAFETY: the caller states `old` is a writable `int *`.
        unsafe { *(old as usize as *mut i32) = darwin };
    }
    err::from_host(rc)
}

/// `int pthread_setcanceltype(int, int *)`, inverted for the same reason.
///
/// # Safety
///
/// As [`setcancelstate`].
pub unsafe extern "C" fn setcanceltype(kind: u64, old: u64) -> i32 {
    let host = match kind as i32 {
        PTHREAD_CANCEL_DEFERRED => host_cancel_deferred(),
        PTHREAD_CANCEL_ASYNCHRONOUS => host_cancel_asynchronous(),
        _ => return err::EINVAL,
    };
    let mut previous = 0;
    // SAFETY: a live local.
    let rc = unsafe { raw::pthread_setcanceltype(host, &mut previous) };
    if rc == 0 && old != 0 {
        let darwin = match previous == host_cancel_deferred() {
            true => PTHREAD_CANCEL_DEFERRED,
            false => PTHREAD_CANCEL_ASYNCHRONOUS,
        };
        // SAFETY: the caller states `old` is a writable `int *`.
        unsafe { *(old as usize as *mut i32) = darwin };
    }
    err::from_host(rc)
}

fn host_cancel_enable() -> libc::c_int {
    raw::cancel::ENABLE
}
fn host_cancel_disable() -> libc::c_int {
    raw::cancel::DISABLE
}
fn host_cancel_deferred() -> libc::c_int {
    raw::cancel::DEFERRED
}
fn host_cancel_asynchronous() -> libc::c_int {
    raw::cancel::ASYNCHRONOUS
}

/// `pthread_key_t` is an `unsigned long` on Darwin and an `unsigned` under musl, so a
/// key crosses as eight bytes and is used as four.
///
/// # Safety
///
/// Called only through an import slot this crate defines.
pub unsafe extern "C" fn key_delete(key: u64) -> i32 {
    match u32::try_from(key) {
        Ok(key) => crate::tls::key::Key::from_index(key).delete(),
        Err(_) => err::EINVAL,
    }
}

/// `int pthread_key_create(pthread_key_t *, void (*destructor)(void *))`
///
/// The entry point the *native* backend binds. Under emulation the destructor is
/// guest code that needs a thunk, so `rosemary::shims::pthread` intercepts the
/// call by name; here the guest's code is host code already.
///
/// The key is written out as **eight** bytes, because that is what the guest
/// reserved: `pthread_key_t` is an `unsigned long` on Darwin and an `unsigned`
/// here. Letting the host write through the guest's pointer fills half of the
/// slot and leaves the other half holding whatever was there before.
///
/// # Safety
///
/// `out` is eight writable bytes and `destructor` is null or a function this
/// process can call.
pub unsafe extern "C" fn key_create(
    out: *mut u64,
    destructor: Option<unsafe extern "C" fn(*mut core::ffi::c_void)>,
) -> i32 {
    if out.is_null() {
        return err::EINVAL;
    }
    // A Darwin key rather than the host's, so the number the guest holds indexes
    // the array its thread pointer addresses. See [`crate::tls::key`].
    match crate::tls::key::Key::create(destructor) {
        Ok(key) => {
            // SAFETY: the caller's eight bytes.
            unsafe { out.write_unaligned(u64::from(key.index())) };
            0
        }
        Err(rc) => rc,
    }
}

/// `int pthread_join(pthread_t, void **)`
///
/// Wrapped for its *result*: the arguments and the handle agree on both
/// platforms, but a failing join returns `ESRCH`, `EDEADLK` or `EINVAL` as a
/// number, and `EDEADLK` is 11 on Darwin and 35 here.
///
/// # Safety
///
/// `thread` is a handle this process's `pthread_create` produced and `value` is
/// a writable pointer slot or null.
pub unsafe extern "C" fn join(thread: u64, value: u64) -> i32 {
    super::idle::reached(super::idle::Idle::Wait);
    err::from_host(unsafe { libc::pthread_join(thread_of(thread), value as usize as *mut _) })
}

/// The stack a guest thread's frames go on when its attributes ask for nothing.
///
/// This host's own default, which is deliberately not the 512 KiB
/// `rosemary::thread::THREAD_STACK` gives an *emulated* guest thread. There the
/// guest's frames have a stack to themselves; here they share one with every shim
/// this crate runs on their behalf, and those are deeper than the Darwin libc the
/// guest was compiled against. A guest reading `pthread_get_stacksize_np` back
/// therefore sees a larger number than it would on a Mac.
const DEFAULT_STACK: usize = 8 * 1024 * 1024;

/// The floor under a guest's own request.
///
/// A natively executed guest runs this runtime's shims on the same stack it runs its
/// own frames on, and those are deeper than the Darwin libc the guest sized itself
/// against. Only ever raises a request, so the thread has at least the room the guest
/// asked for.
const MIN_STACK: usize = 1024 * 1024;

fn stack_size(attributes: &Attributes) -> usize {
    attributes
        .stack_size
        .unwrap_or(DEFAULT_STACK)
        .max(MIN_STACK)
}

/// What [`create`] hands the host thread: the guest's start routine and its argument.
struct Start {
    entry: u64,
    arg: u64,
    qos_class: u32,
    qos_relative_priority: i32,
    registered: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// The host thread's start routine, which enters Darwin code and stays there.
///
/// The guard is held for the whole of the thread's life rather than around a call,
/// because there is no call to wrap: the thread *is* guest code from here to its
/// return, and every host function it invokes is reached from inside it.
///
/// `context` is the [`Start`] [`create`] leaked, handed back unchanged - which is the
/// only value the host can pass, and why this is not an `unsafe fn`: `pthread_create`
/// takes a safe pointer.
extern "C" fn run(context: *mut core::ffi::c_void) -> *mut core::ffi::c_void {
    // Reclaimed and copied out before the guest runs, because a start routine may
    // call `pthread_exit` - which ends the thread from under these frames without
    // unwinding them, leaking anything still boxed. `rosemary::thread::entry` does
    // the same for the same reason.
    // SAFETY: the same allocation, reclaimed once.
    let start = *unsafe { Box::from_raw(context as *mut Start) };
    // SAFETY: this thread is live by definition.
    let qos = u64::from(start.qos_class) | ((start.qos_relative_priority as u32 as u64) << 32);
    thread_ids::register(thread_bits(unsafe { libc::pthread_self() }), id(), qos);
    handle::begin();
    // Before the guest runs: the server may ask this task for something at
    // any moment, and this thread may be the one the signal lands on.
    crate::mach::service::prepare_thread();
    start
        .registered
        .store(true, std::sync::atomic::Ordering::Release);
    let _darwin = crate::tls::DarwinThread::enter();
    if start.qos_class != QOS_CLASS_UNSPECIFIED || start.qos_relative_priority != 0 {
        // SAFETY: values were validated by the attribute setter.
        unsafe { set_qos_class_self_np(start.qos_class, start.qos_relative_priority) };
    }
    // SAFETY: `create` refused a null, and a start routine is `void *(void *)` -
    // one pointer in, one out, which is what this signature is in the guest's
    // integer registers.
    let f: unsafe extern "C" fn(u64) -> u64 =
        unsafe { core::mem::transmute(start.entry as usize as *const ()) };
    // SAFETY: as above; `arg` is the guest's own and only guest code reads it.
    unsafe { f(start.arg) as usize as *mut core::ffi::c_void }
}

/// `int pthread_create(pthread_t *, const pthread_attr_t *, void *(*)(void *), void *)`
///
/// Not passed through, for two reasons that are independent of each other:
///
/// * A Darwin `pthread_attr_t` is [`attr`]'s own record and this host's is a different
///   structure of a different size, so handing the guest's set to the host's
///   `pthread_create` asks it to read a stack size out of bytes that mean something
///   else. This is the [`crate::spawn`] problem again.
/// * A Darwin thread reads `pthread_self`, `errno` and its stack canary as **direct
///   loads off `%gs`** - see [`crate::tls`]. A thread the host started has no Darwin
///   array there, so the guest's start routine runs against whatever `%gs` happens to
///   address. Only [`crate::tls::DarwinThread`] can give it one, and only from inside
///   the new thread.
///
/// # Safety
///
/// `out` is a writable `pthread_t`, `attr` is null or a guest `pthread_attr_t`, and
/// `start` is guest code of this host's architecture.
pub unsafe extern "C" fn create(out: u64, attr: u64, start: u64, arg: u64) -> i32 {
    if out == 0 || start == 0 {
        return err::EINVAL;
    }
    let Some(attributes) = (unsafe { Attributes::of_guest(attr) }) else {
        return err::EINVAL;
    };
    let mut host = match attributes.to_host_attr(stack_size(&attributes)) {
        Ok(host) => host,
        Err(rc) => return rc,
    };
    let registered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let context = Box::into_raw(Box::new(Start {
        entry: start,
        arg,
        qos_class: attributes.qos_class,
        qos_relative_priority: attributes.qos_relative_priority,
        registered: std::sync::Arc::clone(&registered),
    }));
    let mut thread = null_thread();
    // SAFETY: `host` is an initialised attribute set that outlives the call, which is
    // all `pthread_create` needs - it copies what it wants out of one. `run` is this
    // module's own, and owns `context` from here.
    let rc = unsafe {
        libc::pthread_create(
            &mut thread,
            host.as_mut_ptr(),
            run,
            context as *mut core::ffi::c_void,
        )
    };
    if rc != 0 {
        // SAFETY: the thread did not start, so nothing else holds the allocation.
        drop(unsafe { Box::from_raw(context) });
        return err::from_host(rc);
    }
    // Make the cross-thread Darwin id available before `pthread_create` returns.
    // The child publishes it at the first instruction of `run`.
    while !registered.load(std::sync::atomic::Ordering::Acquire) {
        std::thread::yield_now();
    }
    // SAFETY: the caller states `out` is a writable `pthread_t`, which is eight
    // opaque bytes on both platforms - see [`thread_of`], which reads it back.
    unsafe { (out as usize as *mut u64).write(handle::for_host(thread_bits(thread))) };
    0
}

/// `int pthread_detach(pthread_t)`
///
/// # Safety
///
/// As [`join`].
pub unsafe extern "C" fn detach(thread: u64) -> i32 {
    err::from_host(unsafe { libc::pthread_detach(thread_of(thread)) })
}

/// # Safety
///
/// As [`key_delete`].
pub unsafe extern "C" fn getspecific(key: u64) -> u64 {
    // A key nobody created reads null, which is what Darwin does.
    match u32::try_from(key) {
        Ok(key) => crate::tls::key::Key::from_index(key).get() as usize as u64,
        Err(_) => 0,
    }
}

/// # Safety
///
/// As [`key_delete`]; `value` is the guest's own pointer, which only guest code
/// dereferences.
pub unsafe extern "C" fn setspecific(key: u64, value: u64) -> i32 {
    match u32::try_from(key) {
        Ok(key) => {
            crate::tls::key::Key::from_index(key).set(value as usize as *mut core::ffi::c_void)
        }
        Err(_) => err::EINVAL,
    }
}

unsafe extern "C" {
    fn pthread_testcancel();
    fn pthread_getconcurrency() -> libc::c_int;
    fn pthread_setconcurrency(new_level: libc::c_int) -> libc::c_int;
}

exports::exports! {
    functions {
        "pthread_attr_init" => attr::imp::attr_init,
        "pthread_attr_destroy" => attr::imp::attr_destroy,
        "pthread_attr_setdetachstate" => attr::imp::attr_setdetachstate,
        "pthread_attr_getdetachstate" => attr::imp::attr_getdetachstate,
        "pthread_attr_setstacksize" => attr::imp::attr_setstacksize,
        "pthread_attr_getstacksize" => attr::imp::attr_getstacksize,
        "pthread_attr_setstackaddr" => attr::imp::attr_setstackaddr,
        "pthread_attr_getstackaddr" => attr::imp::attr_getstackaddr,
        "pthread_attr_setstack" => attr::imp::attr_setstack,
        "pthread_attr_getstack" => attr::imp::attr_getstack,
        "pthread_attr_setguardsize" => attr::imp::attr_setguardsize,
        "pthread_attr_getguardsize" => attr::imp::attr_getguardsize,
        "pthread_attr_setscope" => attr::imp::attr_setscope,
        "pthread_attr_getscope" => attr::imp::attr_getscope,
        "pthread_attr_setinheritsched" => attr::imp::attr_setinheritsched,
        "pthread_attr_getinheritsched" => attr::imp::attr_getinheritsched,
        "pthread_attr_setschedpolicy" => attr::imp::attr_setschedpolicy,
        "pthread_attr_getschedpolicy" => attr::imp::attr_getschedpolicy,
        "pthread_attr_setschedparam" => attr::imp::attr_setschedparam,
        "pthread_attr_getschedparam" => attr::imp::attr_getschedparam,
        "pthread_attr_set_qos_class_np" => attr::imp::attr_set_qos_class_np,
        "pthread_attr_get_qos_class_np" => attr::imp::attr_get_qos_class_np,

        "pthread_mutex_init" => mutex::mutex_init,
        "pthread_mutex_destroy" => mutex::mutex_destroy,
        "pthread_mutex_lock" => mutex::mutex_lock,
        "pthread_mutex_trylock" => mutex::mutex_trylock,
        "pthread_mutex_unlock" => mutex::mutex_unlock,
        "pthread_mutexattr_init" => mutexattr_init,
        "pthread_mutexattr_destroy" => mutexattr_destroy,
        "pthread_mutexattr_settype" => mutexattr_settype,
        "pthread_mutexattr_gettype" => mutexattr_gettype,
        "pthread_mutexattr_setprotocol" => mutexattr_setprotocol,
        "pthread_mutexattr_getprotocol" => mutexattr_getprotocol,
        "pthread_mutexattr_setpshared" => mutexattr_setpshared,
        "pthread_mutexattr_getpshared" => mutexattr_getpshared,
        "pthread_mutexattr_setpolicy_np" => mutexattr_setpolicy_np,

        "pthread_cond_init" => sync::cond_init,
        "pthread_cond_destroy" => sync::cond_destroy,
        "pthread_cond_wait" => sync::cond_wait,
        "pthread_cond_timedwait" => sync::cond_timedwait,
        "pthread_cond_timedwait_relative_np" => sync::cond_timedwait_relative,
        "pthread_cond_signal" => sync::cond_signal,
        "pthread_cond_broadcast" => sync::cond_broadcast,
        "pthread_condattr_init" => condattr_init,
        "pthread_condattr_destroy" => condattr_destroy,
        "pthread_condattr_setpshared" => condattr_setpshared,
        "pthread_condattr_getpshared" => condattr_getpshared,

        "pthread_rwlock_init" => sync::rwlock_init,
        "pthread_rwlock_destroy" => sync::rwlock_destroy,
        "pthread_rwlock_rdlock" => sync::rwlock_rdlock,
        "pthread_rwlock_tryrdlock" => sync::rwlock_tryrdlock,
        "pthread_rwlock_wrlock" => sync::rwlock_wrlock,
        "pthread_rwlock_trywrlock" => sync::rwlock_trywrlock,
        "pthread_rwlock_unlock" => sync::rwlock_unlock,
        "pthread_rwlockattr_init" => rwlockattr_init,
        "pthread_rwlockattr_destroy" => rwlockattr_destroy,
        "pthread_rwlockattr_setpshared" => rwlockattr_setpshared,
        "pthread_rwlockattr_getpshared" => rwlockattr_getpshared,

        "pthread_key_create" => key_create,
        "pthread_once" => sync::once_entry,
        "pthread_key_delete" => key_delete,
        "pthread_getspecific" => getspecific,
        "pthread_setspecific" => setspecific,

        "pthread_create" => create,
        "pthread_detach" => detach,
        "pthread_kill" => kill,
        "pthread_cancel" => cancel,
        "pthread_get_stackaddr_np" => get_stackaddr_np,
        "pthread_get_stacksize_np" => get_stacksize_np,
        "pthread_setname_np" => setname_np,
        "pthread_getname_np" => getname_np,
        "pthread_main_np" => main_np,
        "pthread_jit_write_protect_np" => jit_write_protect_np,
        "pthread_threadid_np" => threadid_np,
        #[cfg(not(target_os = "macos"))]
        "pthread_cpu_number_np" => cpu_number_np,
        "pthread_mach_thread_np" => mach_thread_np,
        "pthread_yield_np" => yield_np,
        "pthread_set_qos_class_self_np" => set_qos_class_self_np,
        "pthread_get_qos_class_np" => get_qos_class_np,
        "pthread_setcancelstate" => setcancelstate,
        "pthread_setcanceltype" => setcanceltype,

        "pthread_getschedparam" => sched::pthread_getschedparam,
        "pthread_setschedparam" => sched::pthread_setschedparam,

        "pthread_self" => handle::pthread_self,
        "pthread_equal" => handle::pthread_equal,
        "pthread_exit" => libc::pthread_exit,
        "pthread_testcancel" => pthread_testcancel,
        "pthread_getconcurrency" => pthread_getconcurrency,
        "pthread_setconcurrency" => pthread_setconcurrency,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use testing::Guest;

    type Mutex = Guest<{ object::size::MUTEX }>;
    type Cond = Guest<{ object::size::COND }>;
    type RwLock = Guest<{ object::size::RWLOCK }>;
    type Attr = Guest<{ object::size::ATTR }>;
    type MutexAttrObj = Guest<{ object::size::MUTEXATTR }>;
    type CondAttrObj = Guest<{ object::size::CONDATTR }>;
    type RwLockAttrObj = Guest<{ object::size::RWLOCKATTR }>;

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_calls_whose_object_is_darwin_shaped_do_not_bind_to_the_host_s() {
        for name in ["pthread_create", "pthread_once", "pthread_key_create"] {
            let ours = symbol(name).unwrap_or_else(|| panic!("{name} has no entry"));
            assert_ne!(
                Some(ours),
                crate::host_symbol(name),
                "{name} binds to the host's, which reads the guest's object wrong"
            );
        }
    }

    #[test]
    fn every_thread_attribute_reads_back_as_what_was_set() {
        let a = Attr::zeroed();
        // SAFETY: `a` is a live, correctly sized and aligned attribute object.
        unsafe {
            assert_eq!(attr::imp::attr_init(a.at()), 0);

            assert_eq!(
                attr::imp::attr_setdetachstate(a.at(), PTHREAD_CREATE_DETACHED as u64),
                0
            );
            let mut detach = 0i32;
            assert_eq!(
                attr::imp::attr_getdetachstate(a.at(), &mut detach as *mut _ as u64),
                0
            );
            assert_eq!(detach, PTHREAD_CREATE_DETACHED);

            assert_eq!(attr::imp::attr_setstacksize(a.at(), 1 << 20), 0);
            let mut size = 0usize;
            assert_eq!(
                attr::imp::attr_getstacksize(a.at(), &mut size as *mut _ as u64),
                0
            );
            assert_eq!(size, 1 << 20);

            assert_eq!(attr::imp::attr_setguardsize(a.at(), 0), 0);
            let mut guard = 12345usize;
            assert_eq!(
                attr::imp::attr_getguardsize(a.at(), &mut guard as *mut _ as u64),
                0
            );
            assert_eq!(guard, 0, "a guest may ask for no guard at all");

            assert_eq!(
                attr::imp::attr_setscope(a.at(), PTHREAD_SCOPE_PROCESS as u64),
                err::ENOTSUP
            );
            let mut scope = 0i32;
            assert_eq!(
                attr::imp::attr_getscope(a.at(), &mut scope as *mut _ as u64),
                0
            );
            assert_eq!(scope, PTHREAD_SCOPE_SYSTEM);

            assert_eq!(attr::imp::attr_setschedpolicy(a.at(), SCHED_RR as u64), 0);
            let mut policy = 0i32;
            assert_eq!(
                attr::imp::attr_getschedpolicy(a.at(), &mut policy as *mut _ as u64),
                0
            );
            assert_eq!(policy, SCHED_RR, "Darwin's SCHED_RR, not the host's");

            assert_eq!(attr::imp::attr_destroy(a.at()), 0);
        }
    }

    /// The two ends of a supplied stack. `pthread_attr_setstack` takes the low
    /// address and `pthread_attr_getstackaddr` reports the high one, and getting the
    /// direction wrong puts a thread's frames below its own guard page.
    #[test]
    fn a_supplied_stack_is_set_by_its_base_and_read_by_its_top() {
        let a = Attr::zeroed();
        let (base, size) = (0x2_0000_0000u64, 1 << 20);
        // SAFETY: a live attribute object; the stack address is never dereferenced.
        unsafe {
            assert_eq!(attr::imp::attr_init(a.at()), 0);
            assert_eq!(attr::imp::attr_setstack(a.at(), base, size), 0);

            let mut top = 0u64;
            assert_eq!(
                attr::imp::attr_getstackaddr(a.at(), &mut top as *mut _ as u64),
                0
            );
            assert_eq!(top, base + size, "getstackaddr reports the top");

            let (mut low, mut got) = (0u64, 0usize);
            assert_eq!(
                attr::imp::attr_getstack(
                    a.at(),
                    &mut low as *mut _ as u64,
                    &mut got as *mut _ as u64
                ),
                0
            );
            assert_eq!((low, got as u64), (base, size), "getstack reports the base");
        }
    }

    /// A size a guest never set must read back as the size a thread would really get,
    /// because a guest that sizes a buffer off it is owed the truth.
    #[test]
    fn an_unset_stack_size_reads_back_as_darwins_default() {
        let a = Attr::zeroed();
        // SAFETY: a live attribute object.
        unsafe {
            assert_eq!(attr::imp::attr_init(a.at()), 0);
            let mut size = 0usize;
            assert_eq!(
                attr::imp::attr_getstacksize(a.at(), &mut size as *mut _ as u64),
                0
            );
            assert_eq!(size, 512 * 1024, "Darwin's default thread stack");
        }
    }

    #[test]
    fn a_value_darwin_does_not_declare_is_refused() {
        let a = Attr::zeroed();
        // SAFETY: a live attribute object.
        unsafe {
            assert_eq!(attr::imp::attr_init(a.at()), 0);
            // Zero is Linux's `PTHREAD_CREATE_JOINABLE` and is not a Darwin value at
            // all - which is exactly the mistake an untranslated call would make.
            assert_eq!(attr::imp::attr_setdetachstate(a.at(), 0), err::EINVAL);
            assert_eq!(attr::imp::attr_setscope(a.at(), 99), err::EINVAL);
        }
    }

    #[test]
    fn an_uninitialised_attribute_set_is_refused_rather_than_read() {
        let a = Attr::zeroed();
        // SAFETY: a live, zeroed object that was never initialised.
        unsafe {
            let mut out = 0i32;
            assert_eq!(
                attr::imp::attr_getdetachstate(a.at(), &mut out as *mut _ as u64),
                err::EINVAL
            );
            assert_eq!(attr::imp::attr_destroy(a.at()), err::EINVAL);
        }
    }

    #[test]
    fn a_mutex_locks_unlocks_and_is_destroyed() {
        let m = Mutex::zeroed();
        // SAFETY: a live, correctly sized and aligned mutex object.
        unsafe {
            assert_eq!(sync::mutex_init(m.at(), 0), 0);
            assert_eq!(sync::mutex_lock(m.at()), 0);
            assert_eq!(sync::mutex_trylock(m.at()), libc::EBUSY, "already held");
            assert_eq!(sync::mutex_unlock(m.at()), 0);
            assert_eq!(sync::mutex_trylock(m.at()), 0, "and free again");
            assert_eq!(sync::mutex_unlock(m.at()), 0);
            assert_eq!(sync::mutex_destroy(m.at()), 0);
            assert_eq!(sync::mutex_destroy(m.at()), err::EINVAL, "destroyed once");
        }
    }

    #[test]
    fn a_failed_mutex_destroy_leaves_the_mutex_live() {
        let m = Mutex::zeroed();
        unsafe {
            assert_eq!(sync::mutex_init(m.at(), 0), 0);
            assert_eq!(sync::mutex_lock(m.at()), 0);
            assert_eq!(sync::mutex_destroy(m.at()), err::EBUSY);
            assert_eq!(
                sync::mutex_unlock(m.at()),
                0,
                "the host object was restored"
            );
            assert_eq!(sync::mutex_destroy(m.at()), 0, "destroy can be retried");
        }
    }

    /// The bug this whole file exists to prevent: `PTHREAD_MUTEX_RECURSIVE` is 2 on
    /// Darwin and 1 on Linux, so an untranslated request for a recursive lock returns
    /// an error-checking one - which does not recurse, it reports `EDEADLK`.
    #[test]
    fn a_recursive_mutex_recurses_and_an_errorcheck_one_does_not() {
        let attr = MutexAttrObj::zeroed();
        let m = Mutex::zeroed();
        // SAFETY: two live, correctly sized objects.
        unsafe {
            assert_eq!(mutexattr_init(attr.at()), 0);
            assert_eq!(
                mutexattr_settype(attr.at(), PTHREAD_MUTEX_RECURSIVE as u64),
                0
            );
            let mut kind = 0i32;
            assert_eq!(mutexattr_gettype(attr.at(), &mut kind as *mut _ as u64), 0);
            assert_eq!(kind, PTHREAD_MUTEX_RECURSIVE, "Darwin's number, read back");

            assert_eq!(sync::mutex_init(m.at(), attr.at()), 0);
            assert_eq!(sync::mutex_lock(m.at()), 0);
            assert_eq!(sync::mutex_lock(m.at()), 0, "a recursive mutex recurses");
            assert_eq!(sync::mutex_unlock(m.at()), 0);
            assert_eq!(sync::mutex_unlock(m.at()), 0);
            assert_eq!(sync::mutex_destroy(m.at()), 0);

            // And the type Darwin numbers 1, which Linux numbers 2.
            let e = Mutex::zeroed();
            assert_eq!(
                mutexattr_settype(attr.at(), PTHREAD_MUTEX_ERRORCHECK as u64),
                0
            );
            assert_eq!(sync::mutex_init(e.at(), attr.at()), 0);
            assert_eq!(sync::mutex_lock(e.at()), 0);
            // Darwin's number, which is not this host's: a guest compares the
            // result against its own header.
            assert_eq!(
                sync::mutex_lock(e.at()),
                err::EDEADLK,
                "an error-checking mutex reports rather than recursing"
            );
            assert_eq!(sync::mutex_unlock(e.at()), 0);
            assert_eq!(sync::mutex_destroy(e.at()), 0);
            assert_eq!(mutexattr_destroy(attr.at()), 0);
        }
    }

    /// A policy the host cannot honour is still accepted, and one Darwin does not
    /// declare is still refused: the guest checks the return value, not the effect.
    #[test]
    fn a_mutex_policy_is_accepted_and_a_number_darwin_does_not_declare_is_not() {
        let attr = MutexAttrObj::zeroed();
        // SAFETY: a live, correctly sized attribute object.
        unsafe {
            assert_eq!(mutexattr_init(attr.at()), 0);
            assert_eq!(
                mutexattr_setpolicy_np(attr.at(), PTHREAD_MUTEX_POLICY_FIRSTFIT_NP as u64),
                0
            );
            assert_eq!(
                mutexattr_setpolicy_np(attr.at(), PTHREAD_MUTEX_POLICY_FAIRSHARE_NP as u64),
                0
            );
            assert_eq!(mutexattr_setpolicy_np(attr.at(), 2), err::EINVAL);
            assert_eq!(mutexattr_setpolicy_np(0, 1), err::EINVAL);
            assert_eq!(mutexattr_destroy(attr.at()), 0);
            assert_eq!(
                mutexattr_setpolicy_np(attr.at(), 1),
                err::EINVAL,
                "not an attribute set any more"
            );
        }
    }

    #[test]
    fn a_fresh_mutex_attribute_has_darwins_private_default() {
        let attr = MutexAttrObj::zeroed();
        unsafe {
            assert_eq!(mutexattr_init(attr.at()), 0);
            let mut shared = 0;
            assert_eq!(
                mutexattr_getpshared(attr.at(), &mut shared as *mut _ as u64),
                0
            );
            assert_eq!(shared, PTHREAD_PROCESS_PRIVATE);
            let mut protocol = -1;
            assert_eq!(
                mutexattr_getprotocol(attr.at(), &mut protocol as *mut _ as u64),
                0
            );
            assert_eq!(protocol, PTHREAD_PRIO_NONE);
        }
    }

    #[test]
    fn int_attribute_setters_ignore_unspecified_upper_register_bits() {
        let attr = Attr::zeroed();
        unsafe {
            assert_eq!(attr::imp::attr_init(attr.at()), 0);
            let garbage = 0xdead_beef_0000_0000;
            assert_eq!(
                attr::imp::attr_setdetachstate(attr.at(), garbage | PTHREAD_CREATE_DETACHED as u64,),
                0
            );
            assert_eq!(
                attr::imp::attr_setinheritsched(attr.at(), garbage | PTHREAD_EXPLICIT_SCHED as u64,),
                0
            );
        }
    }

    #[test]
    fn qos_attributes_round_trip() {
        let attr = Attr::zeroed();
        unsafe {
            assert_eq!(attr::imp::attr_init(attr.at()), 0);
            assert_eq!(
                attr::imp::attr_set_qos_class_np(attr.at(), QOS_CLASS_UTILITY, -4),
                0
            );
            let (mut class, mut relative) = (0u32, 0i32);
            assert_eq!(
                attr::imp::attr_get_qos_class_np(
                    attr.at(),
                    &mut class as *mut _ as u64,
                    &mut relative as *mut _ as u64,
                ),
                0
            );
            assert_eq!((class, relative), (QOS_CLASS_UTILITY, -4));
        }
    }

    /// `PTHREAD_MUTEX_INITIALIZER` is written by the guest's compiler, so no call of
    /// ours ever sees it happen. The first lock has to notice and build the object.
    #[test]
    fn a_statically_initialised_mutex_works_without_an_init_call() {
        let m = Mutex::statically_initialised(object::sig::MUTEX_INIT);
        // SAFETY: a live mutex object carrying a Darwin static initializer.
        unsafe {
            assert_eq!(sync::mutex_lock(m.at()), 0);
            assert_eq!(sync::mutex_unlock(m.at()), 0);
            assert_eq!(sync::mutex_destroy(m.at()), 0);
        }
    }

    /// And `PTHREAD_RECURSIVE_MUTEX_INITIALIZER` has to build a *recursive* one:
    /// which static initializer the guest wrote is the only record of what it wanted.
    #[test]
    fn a_statically_initialised_recursive_mutex_recurses() {
        let m = Mutex::statically_initialised(object::sig::RECURSIVE_MUTEX_INIT);
        unsafe {
            assert_eq!(sync::mutex_lock(m.at()), 0);
            assert_eq!(
                sync::mutex_lock(m.at()),
                0,
                "the initializer said recursive"
            );
            assert_eq!(sync::mutex_unlock(m.at()), 0);
            assert_eq!(sync::mutex_unlock(m.at()), 0);
        }
    }

    /// libpthread masks a static initializer before it chooses its low-nibble
    /// kind. Old binaries may therefore carry unrelated high bits and still be
    /// accepted as a normal static mutex.
    #[test]
    fn a_compatible_static_mutex_signature_with_legacy_bits_locks() {
        let m = Mutex::statically_initialised(0xfeed_0000_32aa_aba7);
        unsafe {
            assert_eq!(sync::mutex_lock(m.at()), 0);
            assert_eq!(sync::mutex_unlock(m.at()), 0);
            assert_eq!(sync::mutex_destroy(m.at()), 0);
        }
    }

    /// libpthread accepts the static-signature mask for destruction, but lock's
    /// deferred initializer rejects an unknown low-nibble mutex type.
    #[test]
    fn an_unknown_static_mutex_type_fails_lock_but_can_be_destroyed() {
        let m = Mutex::statically_initialised(0x32aa_aba4);
        unsafe {
            assert_eq!(sync::mutex_lock(m.at()), err::EINVAL);
            assert_eq!(sync::mutex_destroy(m.at()), 0);
        }
    }

    /// A statically initialised mutex first touched by several threads at once must
    /// publish one coherent inline record rather than partially initialized state.
    #[test]
    fn racing_threads_build_one_object_for_one_static_mutex() {
        let m = Mutex::statically_initialised(object::sig::MUTEX_INIT);
        let counter = Arc::new(AtomicU32::new(0));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (m, counter) = (Arc::clone(&m), Arc::clone(&counter));
                std::thread::spawn(move || {
                    for _ in 0..2000 {
                        // SAFETY: a live mutex object shared with the other threads,
                        // which is what a guest's global mutex is.
                        unsafe {
                            assert_eq!(sync::mutex_lock(m.at()), 0);
                            let seen = counter.load(Ordering::Relaxed);
                            counter.store(seen + 1, Ordering::Relaxed);
                            assert_eq!(sync::mutex_unlock(m.at()), 0);
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().expect("a locking thread");
        }
        assert_eq!(
            counter.load(Ordering::Relaxed),
            8 * 2000,
            "a lost update means initialization or exclusion split"
        );
    }

    /// An object of one kind handed to another kind's function is `EINVAL`, not a
    /// mutex state built out of a condition variable.
    #[test]
    fn a_cond_is_not_a_mutex() {
        let c = Cond::zeroed();
        // SAFETY: a live, correctly sized condition variable object.
        unsafe {
            assert_eq!(sync::cond_init(c.at(), 0), 0);
            assert_eq!(sync::mutex_lock(c.at()), err::EINVAL);
            assert_eq!(sync::cond_destroy(c.at()), 0);
        }
    }

    /// Copying an initialised mutex is undefined in POSIX, and the copy sharing the
    /// original's state is the quiet way to get it wrong: both would appear to lock
    /// while their address identity no longer agrees.
    #[test]
    fn a_copied_mutex_is_refused_rather_than_aliased() {
        let original = Mutex::zeroed();
        // SAFETY: a live mutex object.
        unsafe { assert_eq!(sync::mutex_init(original.at(), 0), 0) };

        let copy = Mutex::copy_of(&original);
        // SAFETY: a live, correctly sized object holding a byte copy of another's
        // record - which is exactly what the identity word exists to catch.
        unsafe {
            assert_eq!(sync::mutex_lock(copy.at()), err::EINVAL);
            assert_eq!(
                sync::mutex_lock(original.at()),
                0,
                "the original still works"
            );
            assert_eq!(sync::mutex_unlock(original.at()), 0);
            assert_eq!(sync::mutex_destroy(original.at()), 0);
        }
    }

    /// Re-initialising the copy claims that address for a mutex of its own and
    /// leaves the original, which still owns its own state, untouched.
    #[test]
    fn reinitialising_a_copy_leaves_the_original_alone() {
        let original = Mutex::zeroed();
        unsafe { assert_eq!(sync::mutex_init(original.at(), 0), 0) };
        let copy = Mutex::copy_of(&original);
        // SAFETY: two live, correctly sized mutex objects.
        unsafe {
            assert_eq!(sync::mutex_init(copy.at(), 0), 0);
            assert_eq!(sync::mutex_lock(original.at()), 0);
            assert_eq!(sync::mutex_unlock(original.at()), 0);
            assert_eq!(sync::mutex_destroy(copy.at()), 0);
            assert_eq!(sync::mutex_destroy(original.at()), 0);
        }
    }

    #[test]
    fn nonnull_invalid_cond_and_rwlock_attributes_are_refused() {
        let c = Cond::zeroed();
        let l = RwLock::zeroed();
        let ca = CondAttrObj::zeroed();
        let ra = RwLockAttrObj::zeroed();
        unsafe {
            assert_eq!(sync::cond_init(c.at(), ca.at()), err::EINVAL);
            assert_eq!(sync::rwlock_init(l.at(), ra.at()), err::EINVAL);
        }
    }

    #[test]
    fn a_process_shared_mutex_is_refused_rather_than_made_private() {
        let attr = MutexAttrObj::zeroed();
        let m = Mutex::zeroed();
        // SAFETY: two live, correctly sized objects.
        unsafe {
            assert_eq!(mutexattr_init(attr.at()), 0);
            assert_eq!(
                mutexattr_setpshared(attr.at(), PTHREAD_PROCESS_SHARED as u64),
                0
            );
            let mut shared = 0i32;
            assert_eq!(
                mutexattr_getpshared(attr.at(), &mut shared as *mut _ as u64),
                0
            );
            assert_eq!(shared, PTHREAD_PROCESS_SHARED);
            assert_eq!(sync::mutex_init(m.at(), attr.at()), err::ENOTSUP);
        }
    }

    /// Darwin's cancel state values are the opposite way round from the host's, so an
    /// untranslated call disables cancellation where the guest asked to enable it.
    #[test]
    fn cancel_state_round_trips_in_darwins_numbering() {
        // SAFETY: a live local out-parameter.
        unsafe {
            let mut old = -1i32;
            assert_eq!(
                setcancelstate(PTHREAD_CANCEL_DISABLE as u64, &mut old as *mut _ as u64),
                0
            );
            let mut back = -1i32;
            assert_eq!(
                setcancelstate(PTHREAD_CANCEL_ENABLE as u64, &mut back as *mut _ as u64),
                0
            );
            assert_eq!(back, PTHREAD_CANCEL_DISABLE, "what was just set");
            assert_eq!(
                setcancelstate(99, core::ptr::null_mut::<i32>() as u64),
                err::EINVAL
            );
        }
    }

    /// `pthread_kill` carries a Darwin signal number, which is not the host's for
    /// most of the range.
    ///
    /// Asked through the "does this thread exist" probe and through a number the host
    /// has not got, so nothing is ever actually delivered to the test process.
    #[test]
    fn pthread_kill_translates_the_signal_number() {
        // SAFETY: signal zero delivers nothing; the thread is this one.
        assert_eq!(unsafe { kill(this_thread(), 0) }, 0);

        // Where the host has no `SIGINFO`, delivering whatever occupies 29 instead
        // would run a handler installed for something else.
        if crate::signal::unraisable(crate::signal::signum::SIGINFO) {
            assert_eq!(
                unsafe { kill(this_thread(), crate::signal::signum::SIGINFO as u64) },
                err::EINVAL
            );
        }

        // A number that is not a signal on either host.
        // SAFETY: refused before anything is delivered.
        assert_eq!(unsafe { kill(this_thread(), 200) }, err::EINVAL);
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn another_threads_darwin_ids_are_available() {
        use std::sync::atomic::AtomicBool;

        static GO: AtomicBool = AtomicBool::new(false);
        extern "C" fn wait(_: u64) -> u64 {
            while !GO.load(Ordering::Acquire) {
                std::hint::spin_loop();
            }
            0
        }

        GO.store(false, Ordering::Release);
        let mut thread = 0u64;
        unsafe {
            assert_eq!(
                create(&mut thread as *mut _ as u64, 0, wait as *const () as u64, 0,),
                0
            );
            let mut tid = 0u64;
            assert_eq!(threadid_np(thread, &mut tid as *mut _ as u64), 0);
            assert_ne!(tid, 0);
            assert_eq!(u64::from(mach_thread_np(thread)), tid);
            GO.store(true, Ordering::Release);
            assert_eq!(join(thread, 0), 0);
        }
    }

    fn this_thread() -> u64 {
        // SAFETY: `pthread_self` is infallible; the handle is pointer-sized.
        unsafe { core::mem::transmute_copy(&libc::pthread_self()) }
    }

    /// A thread this module starts runs with the *Darwin* thread pointer, which is
    /// the whole reason `pthread_create` is not passed through: a guest reads
    /// `pthread_self` and its stack canary as direct loads off it and never calls
    /// anything this could be done inside.
    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn a_created_thread_runs_with_the_darwin_thread_pointer() {
        use std::sync::atomic::{AtomicU64, Ordering};

        /// `ARCH_GET_GS`, from `<asm/prctl.h>`. Not in the `libc` crate.
        const ARCH_GET_GS: libc::c_int = 0x1004;

        /// The `%gs` *base*, asked for rather than dereferenced: a thread that
        /// never got one would fault on the load a guest actually compiles to,
        /// and a failing test beats a crashing one.
        extern "C" fn body(out: u64) -> u64 {
            let mut base: u64 = 0;
            // SAFETY: `arch_prctl` with `ARCH_GET_GS` writes this thread's `%gs`
            // base through the pointer, which is a live local.
            unsafe { libc::syscall(libc::SYS_arch_prctl, ARCH_GET_GS, &mut base) };
            // SAFETY: the `AtomicU64` the caller passed, live until the join below.
            unsafe { &*(out as usize as *const AtomicU64) }.store(base, Ordering::SeqCst);
            0
        }

        let seen = AtomicU64::new(0);
        let mut thread = null_thread();
        // SAFETY: a writable `pthread_t`, no attributes, and a start routine of
        // this module's own taking the address of a live local.
        let rc = unsafe {
            create(
                &mut thread as *mut _ as u64,
                0,
                body as *const () as u64,
                &seen as *const _ as u64,
            )
        };
        assert_eq!(rc, 0, "the thread did not start");
        // SAFETY: the handle `create` just wrote, joined exactly once.
        assert_eq!(unsafe { join(thread as u64, 0) }, 0);
        assert_ne!(
            seen.load(Ordering::SeqCst),
            0,
            "the new thread ran with no Darwin thread pointer at all"
        );
    }
}
