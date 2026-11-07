//! Darwin's `pthread_key_t`: an index into the thread-specific data array a Darwin
//! thread reaches through its thread pointer.
//!
//! The index is guest-visible in a way a host key is not. libpthread's own
//! `pthread_getspecific` is `tsd[key]`, and dyld's `tlv_get_addr` reads a
//! thread-local's block as `%gs:[key*8]` (`tpidrro_el0` on arm64) without calling
//! anything. A key is only Darwin's if that read finds its value, so where the guest's
//! array is this runtime's own, so are the keys.
//!
//! Three backends, fixed at build time:
//!
//! * **macOS**: the host's keys. The host's array *is* the Darwin array.
//! * **x86-64 Linux and FreeBSD**: an allocator over the synthetic array
//!   [`super::tsd_base`] builds, with libpthread's layout and teardown.
//! * **Anywhere else** (AArch64 Linux): the host's keys, which nothing can read off a
//!   thread pointer - see [`addressable`]. Until the guest's thread pointer can be set
//!   there, a Darwin index would promise a read that does not work.

use core::ffi::c_void;

use crate::pthread::err;

/// A key's destructor: called at thread exit with the thread's non-null value.
pub type Destructor = unsafe extern "C" fn(*mut c_void);

/// The first key `pthread_key_create` hands out. Below it are libpthread's reserved
/// slots and the static keys frameworks claim by number.
pub const FIRST_DYNAMIC: u32 = 256;

/// The first static key, `__TSD_RESERVED_MAX + 1`: libpthread's own slots are
/// below it.
pub const FIRST_STATIC: u32 = 10;

/// One past the last key: `FIRST_DYNAMIC + PTHREAD_KEYS_MAX`.
pub const END: u32 = 768;

/// `PTHREAD_DESTRUCTOR_ITERATIONS`.
pub const DESTRUCTOR_ITERATIONS: usize = 4;

/// One Darwin thread-specific data key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key(u32);

/// Whether a key's value is the word at `index * 8` in the array the guest's thread
/// pointer addresses - which is what lets a descriptor carry a key for `tlv_get_addr`
/// to read directly.
pub const fn addressable() -> bool {
    cfg!(any(
        target_os = "macos",
        all(
            any(target_os = "linux", target_os = "freebsd"),
            target_arch = "x86_64"
        )
    ))
}

impl Key {
    /// A new key, every thread's value for it null.
    pub fn create(destructor: Option<Destructor>) -> Result<Key, i32> {
        imp::create(destructor).map(Key)
    }

    /// The key a guest names by number.
    pub const fn from_index(index: u32) -> Key {
        Key(index)
    }

    /// What a guest's `pthread_key_t` holds for this key.
    pub const fn index(self) -> u32 {
        self.0
    }

    /// `pthread_key_init_np`: give a static key, one a framework claims by
    /// number, its destructor.
    pub fn init_static(self, destructor: Option<Destructor>) -> i32 {
        imp::init_static(self.0, destructor)
    }

    /// Give the key back. Threads' values for it are cleared, not destructed.
    pub fn delete(self) -> i32 {
        imp::delete(self.0)
    }

    /// This thread's value.
    pub fn get(self) -> *mut c_void {
        imp::get(self.0)
    }

    /// Set this thread's value.
    pub fn set(self, value: *mut c_void) -> i32 {
        imp::set(self.0, value)
    }
}

#[cfg(not(all(
    any(target_os = "linux", target_os = "freebsd"),
    target_arch = "x86_64"
)))]
mod imp {
    use super::*;

    pub fn create(destructor: Option<Destructor>) -> Result<u32, i32> {
        let mut key: libc::pthread_key_t = 0;
        // SAFETY: a live local and the caller's destructor.
        let rc = unsafe { libc::pthread_key_create(&mut key, destructor) };
        // `pthread_key_t` is unsigned on Linux and macOS and signed on FreeBSD.
        match rc {
            0 => Ok(key as u32),
            rc => Err(err::from_host(rc)),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn init_static(key: u32, destructor: Option<Destructor>) -> i32 {
        unsafe extern "C" {
            fn pthread_key_init_np(key: libc::c_int, destructor: Option<Destructor>)
            -> libc::c_int;
        }
        // SAFETY: the host validates the key; the destructor is the caller's.
        unsafe { pthread_key_init_np(key as libc::c_int, destructor) }
    }

    /// No static key here can be read off a thread pointer, which is the only
    /// reason a framework claims one by number.
    #[cfg(not(target_os = "macos"))]
    pub fn init_static(_key: u32, _destructor: Option<Destructor>) -> i32 {
        err::ENOTSUP
    }

    pub fn delete(key: u32) -> i32 {
        // SAFETY: a key nobody created is refused by the host, not undefined.
        err::from_host(unsafe { libc::pthread_key_delete(key as libc::pthread_key_t) })
    }

    pub fn get(key: u32) -> *mut c_void {
        // SAFETY: as above; an uncreated key reads null.
        unsafe { libc::pthread_getspecific(key as libc::pthread_key_t) }
    }

    pub fn set(key: u32, value: *mut c_void) -> i32 {
        // SAFETY: as above; the value is the caller's and only stored.
        err::from_host(unsafe { libc::pthread_setspecific(key as libc::pthread_key_t, value) })
    }
}

#[cfg(all(
    any(target_os = "linux", target_os = "freebsd"),
    target_arch = "x86_64"
))]
mod imp {
    use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;

    /// A key no one holds.
    const FREE: usize = 0;
    /// A key held with no destructor. Any other value is the destructor's address.
    const HELD: usize = 1;

    /// One word per dynamic key, which is the whole of the allocator: claiming a
    /// key is a compare-exchange on its word, so creation never blocks a thread
    /// that is reading another key.
    static KEYS: [AtomicUsize; (END - FIRST_DYNAMIC) as usize] =
        [const { AtomicUsize::new(FREE) }; (END - FIRST_DYNAMIC) as usize];

    /// The static keys' destructors, in the same encoding. A static key is
    /// never held or freed: it exists by number.
    static STATIC_KEYS: [AtomicUsize; (FIRST_DYNAMIC - FIRST_STATIC) as usize] =
        [const { AtomicUsize::new(FREE) }; (FIRST_DYNAMIC - FIRST_STATIC) as usize];

    fn word(key: u32) -> Option<&'static AtomicUsize> {
        key.checked_sub(FIRST_DYNAMIC)
            .and_then(|i| KEYS.get(i as usize))
    }

    fn static_word(key: u32) -> Option<&'static AtomicUsize> {
        key.checked_sub(FIRST_STATIC)
            .and_then(|i| STATIC_KEYS.get(i as usize))
    }

    pub fn init_static(key: u32, destructor: Option<Destructor>) -> i32 {
        match static_word(key) {
            Some(w) => {
                w.store(destructor.map_or(FREE, |d| d as usize), Ordering::Release);
                0
            }
            None => err::EINVAL,
        }
    }

    pub fn create(destructor: Option<Destructor>) -> Result<u32, i32> {
        let claim = destructor.map_or(HELD, |d| d as usize);
        (FIRST_DYNAMIC..END)
            .find(|&key| {
                word(key).is_some_and(|w| {
                    w.compare_exchange(FREE, claim, Ordering::AcqRel, Ordering::Relaxed)
                        .is_ok()
                })
            })
            .ok_or(err::EAGAIN)
    }

    /// As libpthread, clears the key's slot in every thread. Programs rely on it:
    /// Feral's thread-locals free their per-thread table and delete its key without
    /// clearing the value, then read the key again.
    pub fn delete(key: u32) -> i32 {
        let Some(w) = word(key).filter(|w| w.load(Ordering::Acquire) != FREE) else {
            return err::EINVAL;
        };
        // Cleared before the key is freed, so `create` never hands out a number
        // with stale values behind it.
        crate::pthread::handle::for_each_block(|block| {
            let offset = crate::pthread::handle::TSD_OFFSET + key as usize * 8;
            let slot = (block as usize + offset) as *mut u64;
            // SAFETY: every block is a leaked `pthread_t` holding `END` slots from
            // `TSD_OFFSET`; another thread may be reading its own slot at the same time.
            unsafe { AtomicU64::from_ptr(slot).store(0, Ordering::Release) };
        });
        match w.swap(FREE, Ordering::AcqRel) != FREE {
            true => 0,
            false => err::EINVAL,
        }
    }

    pub fn get(key: u32) -> *mut c_void {
        if key >= END {
            return core::ptr::null_mut();
        }
        // SAFETY: the thread's own array has `END` words of slots.
        unsafe { *slots().add(key as usize) as usize as *mut c_void }
    }

    pub fn set(key: u32, value: *mut c_void) -> i32 {
        // libpthread's rule: a reserved slot is always settable, a dynamic key
        // only once created.
        let settable = match word(key) {
            Some(w) => w.load(Ordering::Acquire) != FREE,
            None => key < FIRST_DYNAMIC,
        };
        if !settable {
            return err::EINVAL;
        }
        // SAFETY: as in `get`, and `key < END` follows from `settable`.
        unsafe { *slots().add(key as usize) = value as usize as u64 };
        0
    }

    fn slots() -> *mut u64 {
        crate::tls::tsd_base() as usize as *mut u64
    }

    /// libpthread's `_pthread_tsd_cleanup`, on the array at `slots`.
    ///
    /// In key order, each value cleared before its destructor sees it, and again
    /// for as many passes as a destructor keeps setting values - up to
    /// [`DESTRUCTOR_ITERATIONS`]. The dynamic keys go first, then the static
    /// ones, so a framework's static state outlives what the program built on it.
    ///
    /// # Safety
    ///
    /// `slots` is the calling thread's own array, installed as its
    /// [`crate::tls::tsd_base`] for the duration.
    pub(crate) unsafe fn run_destructors(slots: *mut u64) {
        // SAFETY: the caller's guarantee, passed on.
        unsafe {
            run_pass(slots, FIRST_DYNAMIC..END, word);
            run_pass(slots, FIRST_STATIC..FIRST_DYNAMIC, static_word);
        }
    }

    /// # Safety
    ///
    /// As [`run_destructors`].
    unsafe fn run_pass(
        slots: *mut u64,
        keys: core::ops::Range<u32>,
        word: fn(u32) -> Option<&'static AtomicUsize>,
    ) {
        for _ in 0..DESTRUCTOR_ITERATIONS {
            let mut ran = false;
            for key in keys.clone() {
                let Some(w) = word(key) else { continue };
                let destructor = w.load(Ordering::Acquire);
                if destructor == FREE || destructor == HELD {
                    continue;
                }
                // SAFETY: the caller's array, `key < END`.
                let at = unsafe { slots.add(key as usize) };
                let value = unsafe { *at };
                if value == 0 {
                    continue;
                }
                // SAFETY: as above.
                unsafe { *at = 0 };
                // SAFETY: `destructor` was stored from a `Destructor` in `create`.
                let f: Destructor = unsafe { core::mem::transmute(destructor) };
                // A destructor is Darwin code, so it runs with the Darwin array
                // in the thread pointer.
                crate::call_as_darwin!(unsafe { f(value as usize as *mut c_void) });
                ran = true;
            }
            if !ran {
                return;
            }
        }
    }
}

#[cfg(all(
    any(target_os = "linux", target_os = "freebsd"),
    target_arch = "x86_64"
))]
pub(crate) use imp::run_destructors;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_value_is_per_thread() {
        let key = Key::create(None).expect("a key is available");
        assert_eq!(key.set(0x10 as *mut c_void), 0);
        assert_eq!(key.get() as usize, 0x10);
        let theirs = std::thread::spawn(move || key.get() as usize)
            .join()
            .unwrap();
        assert_eq!(theirs, 0, "another thread starts at null");
        assert_eq!(key.delete(), 0);
    }

    #[test]
    #[cfg(any(
        target_os = "macos",
        all(
            any(target_os = "linux", target_os = "freebsd"),
            target_arch = "x86_64"
        )
    ))]
    fn dynamic_keys_are_numbered_as_darwin_numbers_them() {
        let key = Key::create(None).unwrap();
        assert!(
            (FIRST_DYNAMIC..END).contains(&key.index()),
            "key {} is outside libpthread's dynamic range",
            key.index()
        );
        key.delete();
    }

    #[test]
    #[cfg(any(
        target_os = "macos",
        all(
            any(target_os = "linux", target_os = "freebsd"),
            target_arch = "x86_64"
        )
    ))]
    fn the_value_is_the_word_the_thread_pointer_addresses() {
        let key = Key::create(None).unwrap();
        key.set(0x5eed as *mut c_void);
        let base = crate::tls::tsd_base() as usize as *const u64;
        // SAFETY: `key < END`, inside this thread's array.
        assert_eq!(unsafe { *base.add(key.index() as usize) }, 0x5eed);
        key.delete();
    }

    #[test]
    fn deleting_a_key_clears_every_thread_s_value() {
        let key = Key::create(None).unwrap();
        key.set(0x10 as *mut c_void);
        let (set, was_set) = std::sync::mpsc::channel::<()>();
        let (deleted, wait) = std::sync::mpsc::channel::<()>();
        let theirs = std::thread::spawn(move || {
            key.set(0x20 as *mut c_void);
            set.send(()).unwrap();
            wait.recv().unwrap();
            key.get() as usize
        });
        was_set.recv().unwrap();
        assert_eq!(key.delete(), 0);
        deleted.send(()).unwrap();
        assert_eq!(key.get() as usize, 0, "this thread's value is cleared");
        assert_eq!(
            theirs.join().unwrap(),
            0,
            "the other thread's value is cleared"
        );
    }

    #[test]
    fn a_destructor_runs_once_per_thread_with_its_value() {
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn record(value: *mut c_void) {
            SEEN.fetch_add(value as usize, Ordering::SeqCst);
        }
        let key = Key::create(Some(record)).unwrap();
        std::thread::spawn(move || {
            key.set(7 as *mut c_void);
        })
        .join()
        .unwrap();
        assert_eq!(SEEN.load(Ordering::SeqCst), 7);
        key.delete();
    }

    #[test]
    fn a_destructor_that_sets_its_key_again_is_called_again() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        static KEY: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn again(_: *mut c_void) {
            if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
                Key::from_index(KEY.load(Ordering::SeqCst) as u32).set(std::ptr::dangling_mut());
            }
        }
        let key = Key::create(Some(again)).unwrap();
        KEY.store(key.index() as usize, Ordering::SeqCst);
        std::thread::spawn(move || {
            key.set(std::ptr::dangling_mut());
        })
        .join()
        .unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        key.delete();
    }

    #[test]
    #[cfg(all(
        any(target_os = "linux", target_os = "freebsd"),
        target_arch = "x86_64"
    ))]
    fn a_static_key_destructor_runs_at_thread_exit() {
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn record(value: *mut c_void) {
            SEEN.fetch_add(value as usize, Ordering::SeqCst);
        }
        // Swift's first framework key, `__PTK_FRAMEWORK_SWIFT_KEY0`.
        let key = Key::from_index(100);
        assert_eq!(key.init_static(Some(record)), 0);
        std::thread::spawn(move || {
            key.set(5 as *mut c_void);
        })
        .join()
        .unwrap();
        assert_eq!(SEEN.load(Ordering::SeqCst), 5);
        assert_eq!(key.init_static(None), 0);
        assert_eq!(
            Key::from_index(FIRST_DYNAMIC).init_static(None),
            err::EINVAL
        );
    }

    #[test]
    #[cfg(all(
        any(target_os = "linux", target_os = "freebsd"),
        target_arch = "x86_64"
    ))]
    fn an_uncreated_dynamic_key_cannot_be_set() {
        let key = Key::create(None).unwrap();
        assert_eq!(key.delete(), 0);
        assert_eq!(key.set(std::ptr::dangling_mut()), err::EINVAL);
        assert_eq!(key.delete(), err::EINVAL, "a key is deleted once");
    }
}
