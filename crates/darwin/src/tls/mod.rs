mod helpers;
pub mod key;

pub mod slot {
    pub const SELF: usize = 0;
    pub const ERRNO: usize = 1;
    pub const MACH_THREAD_SELF: usize = 3;
    pub const STACK_GUARD: usize = 6;
    pub const PTR_MUNGE: usize = 7;
}

/// libpthread's array: reserved slots, static keys, then `PTHREAD_KEYS_MAX` dynamic
/// keys. See [`key`].
const SLOTS: usize = key::END as usize;

const HOST_THREAD_POINTER: usize = SLOTS;

#[cfg(not(target_os = "macos"))]
const PASSES: usize = SLOTS + 1;

#[cfg(not(target_os = "macos"))]
const WORDS: usize = SLOTS + 2;

#[cfg(not(target_os = "macos"))]
const DESTRUCTOR_PASSES: u64 = 4;

pub const STACK_CHK_GUARD: u64 = 0x00ff_dead_beef_c0de;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn tsd_base() -> u64 {
    let tpidrro: u64;
    unsafe {
        std::arch::asm!("mrs {}, tpidrro_el0", out(reg) tpidrro, options(nomem, nostack, preserves_flags))
    };
    tpidrro & !0b111
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
pub fn tsd_base() -> u64 {
    let base: u64;
    unsafe { std::arch::asm!("mov {}, gs:0", out(reg) base, options(nostack, preserves_flags)) };
    base
}

#[cfg(not(target_os = "macos"))]
thread_local! {
    static BASE: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

#[cfg(not(target_os = "macos"))]
pub fn tsd_base() -> u64 {
    let cached = BASE.with(|base| base.get());
    if cached != 0 {
        return cached;
    }
    let key = synthetic_key();
    // SAFETY: `key` was created by `pthread_key_create` below and is never deleted.
    let existing = unsafe { libc::pthread_getspecific(key) };
    if !existing.is_null() {
        BASE.with(|base| base.set(existing as usize as u64));
        return existing as usize as u64;
    }
    let base = allocate();
    BASE.with(|cell| cell.set(base));
    // SAFETY: as above; `base` is a leaked allocation this thread now owns, and
    // `release` frees it when the thread ends.
    unsafe { libc::pthread_setspecific(key, base as usize as *const core::ffi::c_void) };
    // A new thread starts with its parent's thread pointer - `clone` copies it -
    // so a host thread that runs Darwin code without entering reads the parent's
    // `pthread_self`, thread-locals and allocator identity. Installed here so that
    // the first Darwin call on such a thread ends the sharing. Host code never
    // reads this register.
    #[cfg(all(
        any(target_os = "linux", target_os = "freebsd"),
        target_arch = "x86_64"
    ))]
    if INSTALLED.with(|at| at.get()) == 0 {
        set_thread_pointer(base);
    }
    base
}

#[cfg(not(target_os = "macos"))]
fn synthetic_key() -> libc::pthread_key_t {
    use std::sync::LazyLock;
    static KEY: LazyLock<libc::pthread_key_t> = LazyLock::new(|| {
        let mut key: libc::pthread_key_t = 0;
        // SAFETY: a live local; `release` is an `extern "C"` function of this module.
        let rc = unsafe { libc::pthread_key_create(&mut key, Some(release)) };
        assert_eq!(
            rc, 0,
            "a guest thread cannot run without thread-specific data"
        );
        key
    });
    *KEY
}

#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn release(block: *mut core::ffi::c_void) {
    if block.is_null() {
        return;
    }
    let words = block as *mut u64;
    // Darwin's keys are destructed first, with the array put back so that
    // `tsd_base` - which a destructor reaches through `Key::get` or a guest's
    // `%gs` - finds this array rather than allocating a fresh one.
    // SAFETY: the key is this module's own and `block` is the array it held.
    if unsafe { libc::pthread_setspecific(synthetic_key(), block as *const _) } == 0 {
        // SAFETY: just installed as this thread's array.
        #[cfg(all(
            any(target_os = "linux", target_os = "freebsd"),
            target_arch = "x86_64"
        ))]
        unsafe {
            key::run_destructors(words)
        };
    }
    // SAFETY: the caller's guarantee, on the thread that owns the array.
    let passes = unsafe {
        let at = words.add(PASSES);
        *at += 1;
        *at
    };
    // SAFETY: the key is this module's own, taking back the array it just handed over.
    // Still installed from above for the passes the host has left, so a later
    // host destructor still finds it.
    if passes < DESTRUCTOR_PASSES {
        return;
    }
    // SAFETY: as above; the last pass takes the array away. It is not freed: it
    // lives in the thread's `pthread_t`, which `handle` keeps.
    unsafe { libc::pthread_setspecific(synthetic_key(), core::ptr::null()) };
    BASE.with(|base| {
        if base.get() == block as usize as u64 {
            base.set(0);
        }
    });
}

#[cfg(not(target_os = "macos"))]
const _: () = assert!(
    crate::pthread::handle::TSD_OFFSET + WORDS * 8 <= crate::pthread::handle::SIZE,
    "the array must fit in the thread's pthread_t"
);

/// This thread's array, inside its `pthread_t` as libpthread places it. Zeroed
/// here rather than on release: the block outlives the thread, and glibc hands
/// its control block - and so the block - to a later thread.
#[cfg(not(target_os = "macos"))]
fn allocate() -> u64 {
    let thread = crate::pthread::handle::current();
    let base = thread + crate::pthread::handle::TSD_OFFSET as u64;
    // SAFETY: `handle` leaked a `SIZE`-byte block for this thread, which the
    // assertion above shows holds `WORDS` words from `TSD_OFFSET`.
    let words = unsafe { core::slice::from_raw_parts_mut(base as usize as *mut u64, WORDS) };
    words.fill(0);
    words[slot::SELF] = thread;
    words[slot::ERRNO] = crate::errno::guest_location() as usize as u64;
    words[slot::STACK_GUARD] = STACK_CHK_GUARD;
    words[slot::PTR_MUNGE] = base.rotate_left(17) | 1;
    words[HOST_THREAD_POINTER] = host_thread_pointer();
    base
}

pub fn host_thread_pointer() -> u64 {
    #[cfg(all(not(target_os = "macos"), target_arch = "aarch64"))]
    {
        let tp: u64;
        unsafe {
            std::arch::asm!("mrs {}, tpidr_el0", out(reg) tp, options(nomem, nostack, preserves_flags))
        };
        tp
    }
    #[cfg(all(not(target_os = "macos"), target_arch = "x86_64"))]
    {
        let tp: u64;
        unsafe { std::arch::asm!("mov {}, fs:0", out(reg) tp, options(nostack, preserves_flags)) };
        tp
    }
    #[cfg(any(
        target_os = "macos",
        not(any(target_arch = "aarch64", target_arch = "x86_64"))
    ))]
    {
        0
    }
}

#[macro_export]
macro_rules! call_as_darwin {
    ($($call:tt)*) => {{
        let _darwin = $crate::tls::DarwinThread::enter();
        $($call)*
    }};
}

/// Installs this thread's Darwin array, and leaves it installed.
///
/// Host code never reads the register, so there is nothing to restore - and
/// restoring is what goes wrong: whatever was there before is the parent's
/// array, which `clone` copied and which may since be freed, or nothing. Guest
/// code that runs on this thread later without entering - a key destructor at
/// thread exit, a callback a host library makes on its own - must still find
/// this thread's array, as [`tsd_base`] already arranges for a thread that
/// never entered.
///
/// The guest's syscalls, unlike the register, are interposed only for the
/// scope's life - see [`crate::syscall`].
pub struct DarwinThread {
    _syscalls: crate::syscall::ScopedGuestSyscallContext,
}

pub const fn can_enter_darwin() -> bool {
    cfg!(target_os = "macos")
        || cfg!(all(
            any(target_os = "linux", target_os = "freebsd"),
            target_arch = "x86_64"
        ))
}

const fn swaps() -> bool {
    can_enter_darwin() && !cfg!(target_os = "macos")
}

impl DarwinThread {
    pub fn enter() -> DarwinThread {
        if !entered() {
            set_thread_pointer(tsd_base());
        }
        DarwinThread {
            _syscalls: crate::syscall::ScopedGuestSyscallContext::new(),
        }
    }
}

// FIXME: This abstraction probably won't be used anywhere. Do we just remove it?
pub struct HostThread {
    saved: Option<u64>,
    _syscalls: crate::syscall::ScopedHostSyscallContext,
}

impl HostThread {
    pub fn enter() -> HostThread {
        let syscalls = crate::syscall::ScopedHostSyscallContext::new();
        if !swaps() {
            return HostThread {
                saved: None,
                _syscalls: syscalls,
            };
        }
        let saved = host_thread_pointer();
        let base = tsd_base() as usize as *const u64;
        // SAFETY: `base` is this thread's own array, which has `WORDS` words.
        let host = unsafe { *base.add(HOST_THREAD_POINTER) };
        if host == 0 {
            return HostThread {
                saved: None,
                _syscalls: syscalls,
            };
        }
        set_thread_pointer(host);
        HostThread {
            saved: Some(saved),
            _syscalls: syscalls,
        }
    }
}

impl Drop for HostThread {
    fn drop(&mut self) {
        if let Some(saved) = self.saved {
            set_thread_pointer(saved);
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[inline(always)]
fn set_thread_pointer(base: u64) {
    use crate::tls::helpers::HAS_FSGSBASE;

    if *HAS_FSGSBASE {
        // SAFETY:
        // - CPUID reports FSGSBASE support.
        // - `wrgsbase` changes only this thread's %gs base.
        // - Linux keeps its own thread pointer in %fs, so this does not disturb
        //   the host's TLS.
        unsafe {
            std::arch::asm!(
                "wrgsbase {}",
                in(reg) base,
                options(nostack, preserves_flags),
            );
        }
    } else {
        const ARCH_SET_GS: libc::c_int = 0x1001;

        unsafe {
            libc::syscall(libc::SYS_arch_prctl, ARCH_SET_GS, base);
        }
    }

    INSTALLED.with(|at| at.set(base));
}

#[cfg(all(target_os = "freebsd", target_arch = "x86_64"))]
#[inline(always)]
fn set_thread_pointer(base: u64) {
    unsafe extern "C" {
        fn amd64_set_gsbase(base: *mut libc::c_void) -> libc::c_int;
    }

    let rc = unsafe { amd64_set_gsbase(base as usize as *mut libc::c_void) };
    assert_eq!(rc, 0, "failed to install the Darwin thread pointer");
    INSTALLED.with(|at| at.set(base));
}

#[cfg(all(
    any(target_os = "linux", target_os = "freebsd"),
    target_arch = "x86_64"
))]
thread_local! {
    static INSTALLED: core::cell::Cell<u64> = const { core::cell::Cell::new(0) };
}

pub fn entered() -> bool {
    #[cfg(all(
        any(target_os = "linux", target_os = "freebsd"),
        target_arch = "x86_64"
    ))]
    {
        INSTALLED.with(|at| at.get()) == tsd_base()
    }
    #[cfg(not(all(
        any(target_os = "linux", target_os = "freebsd"),
        target_arch = "x86_64"
    )))]
    {
        !swaps()
    }
}

#[cfg(not(all(
    any(target_os = "linux", target_os = "freebsd"),
    target_arch = "x86_64"
)))]
fn set_thread_pointer(_base: u64) {
    debug_assert!(!swaps(), "a host that swaps must say how");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_s_base_is_stable_and_its_own() {
        let mine = tsd_base();
        assert_ne!(mine, 0);
        assert_eq!(tsd_base(), mine, "a second ask is the same array");

        let theirs = std::thread::spawn(tsd_base)
            .join()
            .expect("a thread has one too");
        assert_ne!(theirs, mine, "two threads must not share an array");
    }

    /// Guest code that runs after the scope, without entering, must still read
    /// this thread's array through `%gs` - not the host's thread pointer.
    #[test]
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn leaving_a_darwin_scope_keeps_the_array_installed() {
        /// `ARCH_GET_GS`, from `<asm/prctl.h>`. Not in the `libc` crate.
        const ARCH_GET_GS: libc::c_int = 0x1004;
        fn gs_base() -> u64 {
            let mut base: u64 = 0;
            // SAFETY: writes this thread's `%gs` base through a live local.
            unsafe { libc::syscall(libc::SYS_arch_prctl, ARCH_GET_GS, &mut base) };
            base
        }
        let (inside, after, own) = std::thread::spawn(|| {
            let inside = {
                let _darwin = DarwinThread::enter();
                gs_base()
            };
            (inside, gs_base(), tsd_base())
        })
        .join()
        .expect("the thread ran");
        assert_eq!(inside, own);
        assert_eq!(after, own, "the scope's end put something else in %gs");
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn slot_zero_is_pthread_self() {
        let base = tsd_base();
        // SAFETY: `base` is this thread's own array, live for the thread.
        let thread = unsafe { *(base as usize as *const u64) };
        assert_eq!(thread, unsafe { crate::pthread::handle::pthread_self() });
    }

    /// Mono's `mono_mach_init` finds a thread's values this way, and falls back to
    /// guessing when no offset it probes holds the value it set.
    #[test]
    #[cfg(all(
        any(target_os = "linux", target_os = "freebsd"),
        target_arch = "x86_64"
    ))]
    fn a_value_is_at_its_key_in_pthread_self() {
        let key = key::Key::create(None).unwrap();
        key.set(0xdead_beef as *mut core::ffi::c_void);
        let thread = unsafe { crate::pthread::handle::pthread_self() } as usize;
        let at = thread + crate::pthread::handle::TSD_OFFSET + key.index() as usize * 8;
        // SAFETY: inside this thread's `pthread_t` block, which is live.
        assert_eq!(unsafe { *(at as *const u64) }, 0xdead_beef);
        key.delete();
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_canary_slot_holds_the_imported_canary() {
        let base = tsd_base() as usize as *const u64;
        // SAFETY: slot 6 is within the array's `SLOTS` words.
        assert_eq!(unsafe { *base.add(slot::STACK_GUARD) }, STACK_CHK_GUARD);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_errno_slot_is_this_thread_s_errno() {
        let base = tsd_base() as usize as *const u64;
        let at = unsafe { *base.add(slot::ERRNO) } as usize as *mut libc::c_int;
        assert_eq!(at, crate::errno::guest_location());
        assert_ne!(
            at,
            crate::errno::location(),
            "the host's numbering is not the guest's"
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn the_array_covers_every_direct_key() {
        assert!(SLOTS >= 256);
        assert!(
            HOST_THREAD_POINTER >= SLOTS,
            "the host's word is past every slot"
        );
        assert!(PASSES > HOST_THREAD_POINTER && WORDS > PASSES);
    }

    #[test]
    fn a_nested_boundary_leaves_the_array_installed() {
        let outer = DarwinThread::enter();
        assert!(entered());
        {
            let _inner = DarwinThread::enter();
        }
        assert!(entered(), "the inner boundary took the array away");
        drop(outer);
    }

    #[test]
    fn the_boundary_puts_the_thread_pointer_back() {
        let before = host_thread_pointer();
        {
            let _darwin = DarwinThread::enter();
        }
        assert_eq!(host_thread_pointer(), before);

        let unwound = std::panic::catch_unwind(|| {
            let _darwin = DarwinThread::enter();
            panic!("out of Darwin code");
        });
        assert!(unwound.is_err());
        assert_eq!(
            host_thread_pointer(),
            before,
            "an unwind must not leave the host's thread pointer replaced"
        );
    }

    #[test]
    fn entering_darwin_code_leaves_the_host_s_own_thread_pointer_alone() {
        let before = host_thread_pointer();
        let guard = DarwinThread::enter();
        assert_eq!(host_thread_pointer(), before);
        drop(guard);
        assert_eq!(host_thread_pointer(), before);
    }

    #[test]
    #[cfg(all(
        any(target_os = "linux", target_os = "freebsd"),
        target_arch = "x86_64"
    ))]
    fn darwin_code_reads_pthread_self_from_gs() {
        let _darwin = DarwinThread::enter();
        let thread: u64;
        unsafe {
            std::arch::asm!("mov {}, gs:0", out(reg) thread, options(nostack, preserves_flags))
        };
        assert_eq!(thread, unsafe { crate::pthread::handle::pthread_self() });
    }
}
