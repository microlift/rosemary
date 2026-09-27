mod helpers;

pub mod slot {
    pub const SELF: usize = 0;
    pub const ERRNO: usize = 1;
    pub const MACH_THREAD_SELF: usize = 3;
    pub const STACK_GUARD: usize = 6;
    pub const PTR_MUNGE: usize = 7;
}

const SLOTS: usize = 512;

/// First word of the block a guest `pthread_key_t` may name.
///
/// # Why the key *is* the word index
///
/// A guest reads its thread-specific values as `gs:[key*8]`, where `key` is
/// whatever `pthread_key_create` returned. That is not an assumption about the
/// ABI, it is what the guest does, measured in the guest's own image: **all
/// 148 `%gs` accesses in Chromium's `Chromium Framework` are of the form
/// `mov rX, QWORD PTR gs:[rY*8+0x0]` with `rY` loaded from a `pthread_key_t`
/// global, and not one of them is a fixed offset.** Every one of the five
/// globals involved is a `pthread_key_t` the guest obtained from the imported
/// `_pthread_key_create`:
///
/// | global | sites | created by |
/// |---|---:|---|
/// | `0xe6a9db0` | 76 | `0x5d69895`, published by the `lock cmpxchg` at `0x5d698f4` |
/// | `0xe6cdd28` | 17 | `0x7c81fa9`, guarded by `cmp QWORD PTR [0xe6cdd28], 0` |
/// | `0xe6cddd0` | 2 | `0x7c828a1`, guarded by `cmp QWORD PTR [0xe6cddd0], 0` |
/// | `0xe6cde78` | 4 | `0x7c8433c`, guarded by `cmp QWORD PTR [0xe6cde78], 0` |
/// | `0xe874fc8` | 49 | `0x5ddf636`, guarded by `cmp BYTE PTR [0xe874fc0], 0` |
///
/// libpthread keeps that array in the thread control block `%gs` points at -
/// which is what `_pthread_tsd_slot(p, MACH_THREAD_SELF)` in
/// `apple-oss-distributions/libpthread` `src/pthread.c` is an index into, and
/// why [`slot`] numbers `SELF` 0, `ERRNO` 1, `MACH_THREAD_SELF` 3,
/// `STACK_GUARD` 6 and `PTR_MUNGE` 7. Those are libpthread's *own* keys, so a
/// key handed to a program has to start above them: [`SPECIFIC_BASE`] is the
/// first word past all of [`slot`], and a key is a plain word index into this
/// block from there on - not a biased one, because a key is only ever compared
/// with itself.
///
/// **A guest that indexes `%gs` at a key this block cannot answer gets a null
/// and dereferences it.** That is not hypothetical: it is the fault Chromium
/// dies on, one call deeper, at `Chromium Framework+0x5d46240`.
#[cfg(not(target_os = "macos"))]
pub const SPECIFIC_BASE: usize = 8;

/// How many guest keys the block has room for.
///
/// [`SLOTS`] less the words libpthread's own keys take. Darwin's
/// `_PTHREAD_KEYS_MAX` is 512, and eight of those are the ones in [`slot`], so
/// this is the same budget under a different name.
#[cfg(not(target_os = "macos"))]
pub const SPECIFIC_SLOTS: usize = SLOTS - SPECIFIC_BASE;

const HOST_THREAD_POINTER: usize = SLOTS;

#[cfg(not(target_os = "macos"))]
const PASSES: usize = SLOTS + 1;

#[cfg(not(target_os = "macos"))]
const WORDS: usize = SLOTS + 2;

#[cfg(not(target_os = "macos"))]
const DESTRUCTOR_PASSES: u64 = 4;

/// The most passes [`release`] will hand the block back for, whatever the guest
/// keys hold.
///
/// glibc calls a key's destructor `PTHREAD_DESTRUCTOR_ITERATIONS` times and
/// then stops, so [`DESTRUCTOR_PASSES`] bounds this in practice. The bound is
/// written down anyway: a re-arm criterion that is a function of what a guest
/// left in the block must not be able to keep a teardown going forever.
#[cfg(not(target_os = "macos"))]
const PASS_LIMIT: u64 = 2 * DESTRUCTOR_PASSES;

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
pub fn tsd_base() -> u64 {
    let key = synthetic_key();
    // SAFETY: `key` was created by `pthread_key_create` below and is never deleted.
    let existing = unsafe { libc::pthread_getspecific(key) };
    if !existing.is_null() {
        return existing as usize as u64;
    }
    let base = allocate();
    // SAFETY: as above; `base` is a leaked allocation this thread now owns, and
    // `release` frees it when the thread ends.
    unsafe { libc::pthread_setspecific(key, base as usize as *const core::ffi::c_void) };
    base
}

#[cfg(not(target_os = "macos"))]
fn synthetic_key() -> libc::pthread_key_t {
    use std::sync::OnceLock;
    static KEY: OnceLock<libc::pthread_key_t> = OnceLock::new();
    *KEY.get_or_init(|| {
        let mut key: libc::pthread_key_t = 0;
        // SAFETY: a live local; `release` is an `extern "C"` function of this module.
        let rc = unsafe { libc::pthread_key_create(&mut key, Some(release)) };
        assert_eq!(
            rc, 0,
            "a guest thread cannot run without thread-specific data"
        );
        key
    })
}

#[cfg(not(target_os = "macos"))]
unsafe extern "C" fn release(block: *mut core::ffi::c_void) {
    if block.is_null() {
        return;
    }
    let words = block as *mut u64;
    // SAFETY: the caller's guarantee, on the thread that owns the array.
    let passes = unsafe {
        let at = words.add(PASSES);
        *at += 1;
        *at
    };
    // A guest key's destructor runs in this same loop, and libpthread runs the
    // keys of a pass in the order it created them - so this block's destructor
    // is reached *before* the guest's. Freeing it here would leave every
    // remaining guest destructor writing into an address nothing holds, which
    // is worse than the leak below: the block is one allocation per thread that
    // ever used a guest `pthread_key_t`, and holding it until the guest's values
    // are gone is what libpthread's own array gets.
    let guest_live = passes < PASS_LIMIT && guest_values_live(words);
    // SAFETY: the key is this module's own, taking back the array it just handed over.
    if (passes < DESTRUCTOR_PASSES || guest_live)
        && unsafe { libc::pthread_setspecific(synthetic_key(), block as *const _) } == 0
    {
        return;
    }
    // SAFETY: as above. Reconstituting the same `Box<[u64]>` `allocate` leaked, with
    // the same length.
    drop(unsafe { Box::from_raw(core::ptr::slice_from_raw_parts_mut(words, WORDS)) });
}

#[cfg(not(target_os = "macos"))]
fn allocate() -> u64 {
    let mut words = vec![0u64; WORDS].into_boxed_slice();
    let base = words.as_ptr() as usize as u64;
    words[slot::SELF] = base;
    words[slot::ERRNO] = crate::errno::guest_location() as usize as u64;
    words[slot::STACK_GUARD] = STACK_CHK_GUARD;
    words[slot::PTR_MUNGE] = base.rotate_left(17) | 1;
    words[HOST_THREAD_POINTER] = host_thread_pointer();
    Box::leak(words);
    base
}

// -------------------------------------------------------------------------------------
// A guest `pthread_key_t`.
//
// The four C names live in [`crate::pthread`], which owns the ABI; what lives here is
// the array the key indexes, because that array is `%gs`. A guest reads it without
// calling anything - `gs:[key*8]`, 148 times in Chromium's Framework - so the word and
// the value it must hold have to be the same memory, and a value written through
// `pthread_setspecific` has to land in the word the next inline read will find.
//
// The host key beside each guest key exists only so that libpthread still runs the
// guest's destructor at thread exit. It is never read for a value: the value is in
// this thread's block, and putting it there is the whole point.
// -------------------------------------------------------------------------------------

/// A guest `pthread_key_t`, and the host key that carries its destructor.
#[cfg(not(target_os = "macos"))]
struct GuestKey {
    /// The word of this thread's block. Also the key as the guest sees it, which
    /// is the only reason the two can be one number.
    word: u64,
    /// This module's `libc::pthread_key_create` key. Its value is kept in step
    /// with [`GuestKey::word`] so that libpthread hands the guest's destructor
    /// the value the guest gave it - and is never read back for one.
    host: libc::pthread_key_t,
}

/// Every key handed to a program, in the order they were handed out, so that the
/// word a key names is the key itself and needs no second number to be found.
#[cfg(not(target_os = "macos"))]
static GUEST_KEYS: std::sync::Mutex<Vec<GuestKey>> = std::sync::Mutex::new(Vec::new());

/// The word `key` names, or `None` for a key this block cannot answer.
///
/// A key is only a word index, so a value nobody handed out names a word that
/// could belong to one of libpthread's own keys - which is a *different* guest's
/// thread-local data, and the reason this returns `None` rather than an address.
#[cfg(not(target_os = "macos"))]
fn specific_word(key: u64) -> Option<usize> {
    let at = key.checked_sub(SPECIFIC_BASE as u64)?;
    (at < SPECIFIC_SLOTS as u64).then_some(SPECIFIC_BASE + at as usize)
}

/// This thread's cell for `key`, or `None` if the key is not one this module
/// handed out.
///
/// The address is a word of this thread's own block, so it is valid for as long
/// as the thread's storage is - see [`release`] for what ends that.
#[cfg(not(target_os = "macos"))]
fn cell(key: u64) -> Option<*mut u64> {
    let at = specific_word(key)?;
    // SAFETY: `tsd_base` is this thread's own array, which has `WORDS` words, and
    // `specific_word` returned an index below `SLOTS`, which is below `WORDS`.
    Some(unsafe { (tsd_base() as usize as *mut u64).add(at) })
}

/// Which entry of [`GUEST_KEYS`] a key is, if it names one.
#[cfg(not(target_os = "macos"))]
fn guest_slot(key: u64) -> Option<usize> {
    let at = key.checked_sub(SPECIFIC_BASE as u64)?;
    (at < SPECIFIC_SLOTS as u64).then_some(at as usize)
}

/// The host key behind `key`, if `key` is one this module handed out.
#[cfg(not(target_os = "macos"))]
fn host_key(key: u64) -> Option<libc::pthread_key_t> {
    let keys = GUEST_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    let entry = keys.get(guest_slot(key)?)?;
    (entry.word == key).then_some(entry.host)
}

/// `int pthread_key_create(pthread_key_t *, void (*)(void *))` - the value half.
///
/// The number written out is a word of `%gs`, which is the only thing that makes
/// the guest's inline `gs:[key*8]` agree with what it was told by
/// [`key_set`].
///
/// # Errors
///
/// `EAGAIN` once the block has no word left. That is the honest answer and it
/// is what libpthread's own `_PTHREAD_KEYS_MAX` exhaustion is: a key that named
/// a word past the end of the array would fault the guest at its next read, so
/// the key is never handed out.
#[cfg(not(target_os = "macos"))]
pub fn key_create(
    destructor: Option<unsafe extern "C" fn(*mut core::ffi::c_void)>,
) -> Result<u64, i32> {
    let mut host: libc::pthread_key_t = 0;
    // SAFETY: a live local, and the caller's destructor.
    let rc = unsafe { libc::pthread_key_create(&mut host, destructor) };
    if rc != 0 {
        return Err(crate::errno::from_host(rc));
    }
    let word = {
        let mut keys = GUEST_KEYS.lock().unwrap_or_else(|e| e.into_inner());
        if keys.len() >= SPECIFIC_SLOTS {
            drop(keys);
            // SAFETY: `host` came from the `pthread_key_create` above and nothing
            // else holds it, so this takes back the only reference.
            unsafe { libc::pthread_key_delete(host) };
            return Err(crate::pthread::consts::err::EAGAIN);
        }
        let word = SPECIFIC_BASE as u64 + keys.len() as u64;
        keys.push(GuestKey { word, host });
        word
    };
    // A key nobody has written has no value, and a word this block used for
    // something else must not answer for this one.
    if let Some(at) = cell(word) {
        // SAFETY: `cell` is a word of this thread's own block.
        unsafe { at.write(0) };
    }
    Ok(word)
}

/// `void *pthread_getspecific(pthread_key_t)`.
///
/// This thread's word, **not** the host key's value. That is deliberate: the
/// guest reads `gs:[key*8]` without calling in, so the two have to be the same
/// cell, and this is what makes them so. They agree because every write goes
/// through [`key_set`], which writes both.
///
/// A key nobody created reads null, which is the whole of the contract a C ABI
/// with no error channel can give.
#[cfg(not(target_os = "macos"))]
pub fn key_get(key: u64) -> u64 {
    if host_key(key).is_none() {
        return 0;
    }
    // SAFETY: `cell` is a word of this thread's own block.
    cell(key).map_or(0, |at| unsafe { at.read() })
}

/// `int pthread_setspecific(pthread_key_t, const void *)`.
///
/// The value is stored verbatim, **tag bits and all**. libpthread's x86-64
/// Darwin TSD word is not a plain pointer: `pthread_self()` is `gs:0 & ~3`, and
/// a program that keeps a heap block in a key stores `block | 3` to say "free
/// this in the destructor" and masks the two low bits off on the way out -
/// Chromium does exactly that at `Chromium Framework+0x5d69970`, storing
/// `block | 3`, and again at `+0x5d69483`, reading `value & ~3`. Masking the
/// tag off here would lose the distinction the guest's own destructor is
/// written against.
///
/// # Errors
///
/// `EINVAL` for a key this module did not hand out. Writing the word such a key
/// names would overwrite another program's thread-local data.
#[cfg(not(target_os = "macos"))]
pub fn key_set(key: u64, value: u64) -> i32 {
    let Some(host) = host_key(key) else {
        return crate::pthread::consts::err::EINVAL;
    };
    if let Some(at) = cell(key) {
        // SAFETY: `cell` is a word of this thread's own block.
        unsafe { at.write(value) };
    }
    // SAFETY: `host` came from `key_create` above and is this module's own key.
    crate::errno::from_host(unsafe {
        libc::pthread_setspecific(host, value as usize as *const core::ffi::c_void)
    })
}

/// `int pthread_key_delete(pthread_key_t)`.
///
/// # Errors
///
/// `EINVAL` for a key this module did not hand out, or one already deleted -
/// libpthread's own `pthread_key_delete` is undefined for a key it did not
/// create, so refusing is the whole of the safety here.
#[cfg(not(target_os = "macos"))]
pub fn key_delete(key: u64) -> i32 {
    let taken = {
        let mut keys = GUEST_KEYS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(at) = guest_slot(key) else {
            return crate::pthread::consts::err::EINVAL;
        };
        match keys.get(at) {
            Some(k) if k.word == key => keys.swap_remove(at),
            _ => return crate::pthread::consts::err::EINVAL,
        }
    };
    // The word is about to be handed out again, so it must not answer for the
    // value this key left in it.
    if let Some(at) = cell(taken.word) {
        // SAFETY: `cell` is a word of this thread's own block.
        unsafe { at.write(0) };
    }
    // SAFETY: `taken.host` came from `key_create` and `taken` is this module's
    // only record of it, so this is the only reference.
    crate::errno::from_host(unsafe { libc::pthread_key_delete(taken.host) })
}

/// Whether any key this module handed out still has a value in `words`.
///
/// For [`release`] only, and the reason it takes the block rather than asking
/// for it: the block is the one thing that must not be freed while a guest
/// destructor is still to run, and calling back into [`tsd_base`] from a
/// destructor to find out would allocate a second one.
///
/// Reads at most the words a key could have named, and takes the count from the
/// registry, so a poisoned lock or a shorter block cannot index past either.
#[cfg(not(target_os = "macos"))]
fn guest_values_live(words: *mut u64) -> bool {
    let keys = GUEST_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: the caller's guarantee, on the thread that owns the array, which
    // has `WORDS` words; the bound is below both `WORDS` and `SLOTS`.
    let words = unsafe { std::slice::from_raw_parts(words, WORDS) };
    words[SPECIFIC_BASE..SPECIFIC_BASE + keys.len().min(SPECIFIC_SLOTS)]
        .iter()
        .any(|v| *v != 0)
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

pub struct DarwinThread {
    saved: Option<u64>,
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
        if entered() {
            return DarwinThread { saved: None };
        }
        let saved = host_thread_pointer();
        set_thread_pointer(tsd_base());
        DarwinThread { saved: Some(saved) }
    }
}

impl Drop for DarwinThread {
    fn drop(&mut self) {
        if let Some(saved) = self.saved {
            set_thread_pointer(saved);
        }
    }
}

pub struct HostThread {
    saved: Option<u64>,
}

impl HostThread {
    pub fn enter() -> HostThread {
        if !swaps() {
            return HostThread { saved: None };
        }
        let saved = host_thread_pointer();
        let base = tsd_base() as usize as *const u64;
        // SAFETY: `base` is this thread's own array, which has `WORDS` words.
        let host = unsafe { *base.add(HOST_THREAD_POINTER) };
        if host == 0 {
            return HostThread { saved: None };
        }
        set_thread_pointer(host);
        HostThread { saved: Some(saved) }
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

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn slot_zero_is_the_array_itself() {
        let base = tsd_base();
        // SAFETY: `base` is this thread's own array, live for the thread.
        assert_eq!(unsafe { *(base as usize as *const u64) }, base);
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
    fn darwin_code_finds_its_array_in_gs() {
        let _darwin = DarwinThread::enter();
        let base: u64;
        unsafe {
            std::arch::asm!("mov {}, gs:0", out(reg) base, options(nostack, preserves_flags))
        };
        assert_eq!(
            base,
            tsd_base(),
            "slot zero of the array in `%gs` is the array"
        );
    }

    /// The whole ABI, end to end, through the instruction the guest uses.
    ///
    /// The guest does not call anything to read a thread-local: it loads the key
    /// and reads `gs:[key*8]`. So a runtime that stores the value anywhere else
    /// hands the guest a null and the guest dereferences it - which is the fault
    /// at `Chromium Framework+0x5d46240`, one call downstream of the accessor
    /// that returned the null. This is the one test in the crate that would
    /// catch it, and it is written as the guest's own instruction rather than as
    /// a call into this module, because a call would pass with the value in the
    /// wrong place.
    #[test]
    #[cfg(all(
        any(target_os = "linux", target_os = "freebsd"),
        target_arch = "x86_64",
        not(target_os = "macos")
    ))]
    fn a_guest_key_is_the_word_gs_indexed_by_the_key_holds() {
        let key = key_create(None).expect("a block with 504 spare words");
        // A value with the tag bits a real Darwin TSD word carries, because
        // `pthread_self()` is `gs:0 & ~3` and a guest that keeps a heap block in
        // a key stores `block | 3` to mark it freeable.
        let value = 0x5eed_0000_0000_0003u64;
        assert_eq!(key_set(key, value), 0);
        assert_eq!(key_get(key), value, "the value is stored verbatim");

        let _darwin = DarwinThread::enter();
        let base: u64;
        let index: u64;
        // SAFETY: `gs:[rax*8]` is one load, and the boundary above installed this
        // thread's own block, which has `WORDS` words and `key` below `SLOTS`.
        unsafe {
            std::arch::asm!(
                "mov {base}, gs:0",
                "mov {index}, {key}",
                "mov {index}, gs:[{index}*8+0x0]",
                base = out(reg) base,
                index = out(reg) index,
                key = in(reg) key,
                options(nostack, preserves_flags),
            )
        };
        assert_eq!(base, tsd_base(), "`%gs` is this thread's block");
        assert_eq!(index, value, "the guest's own load finds what it stored");
        assert_eq!(index & !3, 0x5eed_0000_0000_0000, "and masks off the tag");
    }

    /// A key is a word index, so it must not be one of libpthread's own.
    ///
    /// Not a style point: the guest reads the word and nothing checks what is in
    /// it, so a key that landed on `ERRNO` or `PTR_MUNGE` would hand another
    /// program's thread-local data to a program that believes it is its own.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_guest_key_never_names_a_slot_this_module_uses() {
        for used in [
            slot::SELF,
            slot::ERRNO,
            slot::MACH_THREAD_SELF,
            slot::STACK_GUARD,
            slot::PTR_MUNGE,
        ] {
            assert!(
                used < SPECIFIC_BASE,
                "slot {used} is inside the region a guest key is handed out from"
            );
        }
        assert!(SPECIFIC_BASE + SPECIFIC_SLOTS == SLOTS);
        assert!(
            SPECIFIC_BASE + SPECIFIC_SLOTS <= HOST_THREAD_POINTER,
            "the last guest word is still inside the block"
        );
    }

    /// Every key gets a word of its own, and the first is past libpthread's.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn keys_name_distinct_words_and_none_of_them_is_zero() {
        let a = key_create(None).expect("a key");
        let b = key_create(None).expect("another key");
        assert_ne!(a, b, "a key is not handed out twice");
        assert!(a >= SPECIFIC_BASE as u64 && b >= SPECIFIC_BASE as u64);
        assert_ne!(a, 0, "a key of zero is a program's 'not created yet'");
        assert_ne!(b, 0);
        assert_eq!(key_set(a, 0xa1a1), 0);
        assert_eq!(key_set(b, 0xb2b2), 0);
        assert_eq!(key_get(a), 0xa1a1, "one key's value is not another's");
        assert_eq!(key_get(b), 0xb2b2);
    }

    /// A word handed out again must not answer for the value the last key left
    /// in it. `key_delete` is the only place that can happen, and the guest's
    /// own `pthread_key_delete` dance (a create, a delete, a create again, at
    /// `Chromium Framework+0x5d69887`) reaches it on a live thread.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_deleted_key_reads_as_no_value_so_its_word_can_be_handed_out_again() {
        let key = key_create(None).expect("a key");
        assert_eq!(key_set(key, 0xdead), 0);
        assert_eq!(key_delete(key), 0);
        assert_eq!(key_get(key), 0, "a deleted key has no value");
        assert_eq!(
            key_set(key, 1),
            crate::pthread::consts::err::EINVAL,
            "and refuses to be written"
        );
        assert_eq!(key_delete(key), crate::pthread::consts::err::EINVAL);
    }

    /// Per-thread, like the block it is a word of. A key is process-wide; its
    /// value is not, and a runtime that shared one is the bug a second thread
    /// finds and a single-threaded test never will.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn two_threads_have_two_own_values_for_one_key() {
        let key = key_create(None).expect("a key");
        assert_eq!(key_set(key, 0x1111), 0);
        let theirs = std::thread::spawn(move || {
            let fresh = key_get(key);
            key_set(key, 0x2222);
            (fresh, key_get(key))
        })
        .join()
        .expect("the other thread reads the same key");
        assert_eq!(theirs.0, 0, "a fresh thread's word has no value");
        assert_eq!(theirs.1, 0x2222);
        assert_eq!(key_get(key), 0x1111, "and this one's is untouched");
    }

    /// A key nobody created must not be answered from a word that happens to
    /// look like one. A key is handed out from [`SPECIFIC_BASE`] upwards, so
    /// every word below it - and `u64::MAX`, which names nothing - is a key no
    /// `key_create` can ever return, however many other tests have run first.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_key_nobody_created_is_refused_rather_than_guessed() {
        // Words a key can never be handed out from: `key_create` starts at
        // `SPECIFIC_BASE` and counts up, so however many other tests have run
        // first, none of these is a key. `u64::MAX` names no word at all.
        for key in [0, 1, slot::MACH_THREAD_SELF as u64, SPECIFIC_BASE as u64 - 1] {
            assert!(key < SPECIFIC_BASE as u64);
            assert_eq!(key_get(key), 0, "key {key} was never handed out");
            assert_eq!(
                key_set(key, 0x5eed),
                crate::pthread::consts::err::EINVAL,
                "and refuses a value"
            );
            assert_eq!(
                key_delete(key),
                crate::pthread::consts::err::EINVAL,
                "and refuses to be deleted"
            );
        }
        assert_eq!(key_get(u64::MAX), 0);
        assert_eq!(key_set(u64::MAX, 1), crate::pthread::consts::err::EINVAL);
        assert_eq!(key_delete(u64::MAX), crate::pthread::consts::err::EINVAL);
    }

    /// The teardown interaction, checked from the side that has to hold: while
    /// a guest key still has a value, the block is not free, because a guest
    /// destructor in the same pass writes into it.
    #[test]
    #[cfg(not(target_os = "macos"))]
    fn a_block_holding_a_guest_value_is_not_handed_back_for_freeing() {
        let base = tsd_base() as usize as *mut u64;
        let key = key_create(None).expect("a key");
        // SAFETY: `base` is this thread's own block, which has `WORDS` words.
        unsafe {
            assert!(
                !guest_values_live(base),
                "nothing is written yet"
            );
            assert_eq!(key_set(key, 1), 0);
            assert!(guest_values_live(base), "a value keeps the block");
            assert_eq!(key_set(key, 0), 0);
            assert!(!guest_values_live(base), "clearing it releases the block");
        }
    }
}
