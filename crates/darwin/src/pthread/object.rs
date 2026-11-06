use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Darwin's object sizes on LP64, from `<sys/_pthread/_pthread_types.h>`: the `long`
/// signature plus the `__opaque` block each type declares.
pub mod size {
    pub const ATTR: usize = 8 + 56;
    pub const MUTEX: usize = 8 + 56;
    pub const MUTEXATTR: usize = 8 + 8;
    pub const COND: usize = 8 + 40;
    pub const CONDATTR: usize = 8 + 8;
    pub const RWLOCK: usize = 8 + 192;
    pub const RWLOCKATTR: usize = 8 + 16;
    pub const ONCE: usize = 8 + 8;
}

/// The signature each static initializer writes, from `<pthread/pthread_impl.h>`.
///
/// A guest that writes `PTHREAD_MUTEX_INITIALIZER` into a global has already done the
/// only initialisation it intends to do, so these are the values that must be
/// recognised and turned into a real object on first use.
pub mod sig {
    pub const MUTEX_INIT: u64 = 0x32AA_ABA7;
    pub const ERRORCHECK_MUTEX_INIT: u64 = 0x32AA_ABA1;
    pub const RECURSIVE_MUTEX_INIT: u64 = 0x32AA_ABA2;
    pub const FIRSTFIT_MUTEX_INIT: u64 = 0x32AA_ABA3;
    /// `_PTHREAD_MUTEX_SIG_init_MASK`: preserves the static mutex kind.
    pub const MUTEX_INIT_MASK: u64 = 0xFFFF_FFF0;
    /// Bits of a static initializer that select its mutex kind.
    pub const MUTEX_INIT_TYPE_MASK: u64 = 0x0000_000F;
    /// `_PTHREAD_MUTEX_SIG_init_CMP`: the static mutex signature family.
    pub const MUTEX_INIT_CMP: u64 = 0x32AA_ABA0;
    /// The low-nibble static type values `_pthread_mutex_init` recognises.
    pub const MUTEX_INIT_ERRORCHECK: u64 = ERRORCHECK_MUTEX_INIT & MUTEX_INIT_TYPE_MASK;
    pub const MUTEX_INIT_RECURSIVE: u64 = RECURSIVE_MUTEX_INIT & MUTEX_INIT_TYPE_MASK;
    pub const MUTEX_INIT_FIRSTFIT: u64 = FIRSTFIT_MUTEX_INIT & MUTEX_INIT_TYPE_MASK;
    pub const MUTEX_INIT_NORMAL: u64 = MUTEX_INIT & MUTEX_INIT_TYPE_MASK;
    pub const COND_INIT: u64 = 0x3CB0_B1BB;
    pub const ONCE_INIT: u64 = 0x30B1_BCBA;
    pub const RWLOCK_INIT: u64 = 0x2DA8_B3B4;
}

/// Which kind of object a record describes.
///
/// Carried in the signature so that a `pthread_cond_t` handed to `pthread_mutex_lock`
/// is `EINVAL` rather than being interpreted as mutex state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Mutex = 1,
    Cond = 2,
    RwLock = 3,
    Once = 4,
    Attr = 5,
    MutexAttr = 6,
    CondAttr = 7,
    RwLockAttr = 8,
}

/// The high bytes of every signature this crate writes: `"rm"`, then the kind.
///
/// Chosen so it cannot collide with a Darwin signature, all of which fit in 32 bits,
/// and so it is legible in a hex dump of guest memory.
const TAG: u64 = 0x726d_0000_0000_0000;

/// The signature an initialised object of `kind` carries.
pub const fn ours(kind: Kind) -> u64 {
    TAG | ((kind as u64) << 8) | 0x01
}

/// The signature a record carries while one thread is building or retiring it.
///
/// Only ever observed by another thread racing to touch the same object, which spins
/// until it is gone. Short by construction: what happens under it is a handful of
/// stores to the guest's own bytes, with no allocation, no call into the guest and no
/// lock of ours held.
pub(crate) const BUSY: u64 = TAG | 0xFF;

/// A destroyed object. Distinct from zero so a double destroy is `EINVAL` rather than
/// a fresh lazy initialisation of whatever the memory now holds.
pub(crate) const DEAD: u64 = TAG | 0xDE;

/// One object's record, laid over the guest's bytes.
///
/// ```text
/// +0             u64  signature   ours(kind), a Darwin static initializer, BUSY, or DEAD
/// +8             u64  identity    the guest address this record was built at
/// +[BODY]..      ..   body        whatever the kind makes of the rest
/// ```
///
/// The `identity` word records where the record was built. All of an object's
/// state is in its own bytes and every wait is keyed by its own address, so a
/// byte copy is a second, independent object - which is what it is on Darwin
/// too, and what a guest that copied one relies on. A copy is adopted on first
/// touch and noted once, since POSIX does not promise it works.
///
/// `Once`, `MutexAttr` and `CondAttr` are sixteen bytes and have no room for it.
pub struct Record {
    at: u64,
    kind: Kind,
}

/// Byte offsets within a record.
const SIGNATURE: u64 = 0;
const IDENTITY: u64 = 8;

impl Kind {
    /// Whether an object of this kind has room for the [`IDENTITY`] word.
    const fn tracks_identity(self) -> bool {
        matches!(self, Kind::Mutex | Kind::Cond | Kind::RwLock)
    }

    /// Where this kind's own state begins, after the words the lifecycle claims.
    const fn body(self) -> u64 {
        match self.tracks_identity() {
            true => 16,
            false => 8,
        }
    }

    /// The signature a static initializer for this kind writes, if it has one.
    pub(crate) fn is_static_initializer(self, signature: u64) -> bool {
        match self {
            // libpthread does not compare a static mutex initializer exactly.
            // Its mask preserves the low nibble for `_pthread_mutex_init`, which
            // selects the mutex kind, and ignores the rest. This accommodates
            // older binaries whose initializer carried otherwise-unused bits.
            // Keep that compatibility rule here rather than requiring one of
            // the four initializer spellings current headers expose.
            Kind::Mutex => (signature & sig::MUTEX_INIT_MASK) == sig::MUTEX_INIT_CMP,
            Kind::Cond => signature == sig::COND_INIT,
            Kind::RwLock => signature == sig::RWLOCK_INIT,
            // Darwin's implementation ignores `__sig`, so a zero-filled control
            // word is also a valid never-run once object.
            Kind::Once => matches!(signature, sig::ONCE_INIT | 0),
            // The attribute types have no static initializer: POSIX requires
            // `pthread_attr_init` and friends, so an attribute set this crate has not
            // seen is not one at all.
            _ => false,
        }
    }
}

/// What a lookup found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// An initialised object of this kind, built at this address.
    Live,
    /// A static initializer, not yet turned into an object. The signature says
    /// *which* - a recursive mutex and a plain one are different objects.
    Fresh(u64),
    /// Not an object of this kind, or one that has been destroyed.
    Invalid,
}

impl Record {
    /// The record at guest address `at`.
    ///
    /// # Safety
    ///
    /// `at` is a writable, eight-byte-aligned guest address with at least this kind's
    /// size behind it - which is what a `pthread_mutex_t *` from the guest is.
    pub unsafe fn at(at: u64, kind: Kind) -> Record {
        Record { at, kind }
    }

    fn word(&self, offset: u64) -> &AtomicU64 {
        // SAFETY: the constructor's guarantee. A `pthread_*_t` begins with a `long`,
        // so the object is at least eight-byte aligned, and every offset used here is
        // within the size the kind declares.
        unsafe { &*((self.at + offset) as usize as *const AtomicU64) }
    }

    /// An eight-byte field of this kind's body, at `offset` bytes into it.
    pub fn u64(&self, offset: u64) -> &AtomicU64 {
        debug_assert!(offset.is_multiple_of(8), "an eight-byte field is aligned");
        self.word(self.kind.body() + offset)
    }

    /// A four-byte field of this kind's body, at `offset` bytes into it.
    pub fn u32(&self, offset: u64) -> &AtomicU32 {
        debug_assert!(offset.is_multiple_of(4), "a four-byte field is aligned");
        // SAFETY: as [`Record::word`]; the body begins eight-byte aligned and `offset`
        // is a multiple of four.
        unsafe { &*((self.at + self.kind.body() + offset) as usize as *const AtomicU32) }
    }

    /// The word an object whose whole state is one word keeps it in.
    pub fn cell(&self) -> &AtomicU64 {
        self.u64(0)
    }

    /// This object's state, without changing it.
    ///
    /// A `BUSY` record reports [`State::Fresh`] of nothing useful only to a caller
    /// that is willing to retry; [`Record::live_or_init`] and [`Record::seize`] are
    /// the ones that wait it out.
    pub fn peek(&self) -> State {
        self.classify(self.word(SIGNATURE).load(Ordering::Acquire))
    }

    fn classify(&self, signature: u64) -> State {
        if signature == ours(self.kind) {
            self.adopt();
            return State::Live;
        }
        match self.kind.is_static_initializer(signature) {
            true => State::Fresh(signature),
            false => State::Invalid,
        }
    }

    /// Claim a byte copy of a record built elsewhere for this address.
    fn adopt(&self) {
        if !self.kind.tracks_identity() {
            return;
        }
        let word = self.word(IDENTITY);
        let identity = word.load(Ordering::Relaxed);
        if identity != self.at {
            say_copied(self.kind, self.at, identity);
            // Racing adopters all store the same value.
            let _ = word.compare_exchange(identity, self.at, Ordering::Relaxed, Ordering::Relaxed);
        }
    }

    /// Whether this record is an initialised object of its kind.
    pub fn is_ours(&self) -> bool {
        self.peek() == State::Live
    }

    /// Fill this object's body and publish it as live.
    ///
    /// The signature is stored **last** and with release ordering, so a thread that
    /// observes it observes the body and the identity too.
    pub fn publish(&self, fill: impl FnOnce(&Record)) {
        fill(self);
        if self.kind.tracks_identity() {
            self.word(IDENTITY).store(self.at, Ordering::Relaxed);
        }
        self.word(SIGNATURE)
            .store(ours(self.kind), Ordering::Release);
    }

    /// Make sure this is a live object, building it from a static initializer if that
    /// is all the guest ever wrote.
    ///
    /// `init` is called at most once per object, with the signature of whichever
    /// static initializer it was, and by exactly one thread; a second thread arriving
    /// meanwhile waits rather than building a second object. Returning `false` means
    /// the initializer named something this crate does not implement, and leaves the
    /// object as it was so a caller that can afford to retry is not stuck with one it
    /// can never use.
    pub fn live_or_init(&self, init: impl FnOnce(&Record, u64) -> bool) -> bool {
        loop {
            let signature = self.word(SIGNATURE).load(Ordering::Acquire);
            if signature == BUSY {
                std::hint::spin_loop();
                continue;
            }
            match self.classify(signature) {
                State::Live => return true,
                State::Invalid => return false,
                State::Fresh(_) => {}
            }
            // Claim it. A loser retries and sees `BUSY` or the finished object.
            if self
                .word(SIGNATURE)
                .compare_exchange(signature, BUSY, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                continue;
            }
            let built = init(self, signature);
            if built {
                self.publish(|_| {});
            } else {
                self.word(SIGNATURE).store(signature, Ordering::Release);
            }
            return built;
        }
    }

    /// Take the lifecycle word for a destroy or a re-initialisation, leaving the
    /// record `BUSY` until [`Record::restore`] or [`Record::bury`].
    ///
    /// [`State::Invalid`] claims nothing and needs neither call.
    pub fn seize(&self) -> State {
        loop {
            let signature = self.word(SIGNATURE).load(Ordering::Acquire);
            if signature == BUSY {
                std::hint::spin_loop();
                continue;
            }
            let state = self.classify(signature);
            if state == State::Invalid {
                return state;
            }
            if self
                .word(SIGNATURE)
                .compare_exchange(signature, BUSY, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return state;
            }
        }
    }

    /// Undo a [`Record::seize`], leaving the object exactly as it was found.
    pub fn restore(&self, state: State) {
        let signature = match state {
            State::Live => ours(self.kind),
            State::Fresh(signature) => signature,
            State::Invalid => return,
        };
        self.word(SIGNATURE).store(signature, Ordering::Release);
    }

    /// End a [`Record::seize`] by marking the object destroyed.
    pub fn bury(&self) {
        self.word(SIGNATURE).store(DEAD, Ordering::Release);
    }

    /// One eight-byte slot of this object, counted from its signature.
    ///
    /// Used by the thread-attribute set, which is a plain block of settings rather
    /// than a lock with a body of its own.
    ///
    /// # Panics
    ///
    /// If the slot is past the object, which is a bug in this crate rather than
    /// anything the guest can cause: the sizes are compile-time constants and the
    /// callers are all in this module's neighbours.
    pub fn slot(&self, index: usize, of_size: usize) -> &AtomicU64 {
        assert!(
            (index + 1) * 8 <= of_size,
            "slot {index} is past a {of_size}-byte object"
        );
        self.word(index as u64 * 8)
    }

    /// This object's guest address, which is its identity.
    pub fn address(&self) -> u64 {
        self.at
    }
}

/// Publish an attribute set, or any other object whose whole state is one word.
pub fn publish_cell(record: &Record, value: u64) {
    record.publish(|r| r.cell().store(value, Ordering::Relaxed));
}

/// Destroy an object that owns nothing, reporting Darwin's `EINVAL` if it was not one.
pub fn destroy_plain(record: &Record) -> i32 {
    match record.seize() {
        State::Invalid => super::consts::err::EINVAL,
        _ => {
            record.bury();
            0
        }
    }
}

/// Report that an object was copied after being initialised, once per process.
///
/// It works, as on Darwin, but it is outside POSIX and worth knowing about.
pub(crate) fn say_copied(kind: Kind, at: u64, identity: u64) {
    use std::sync::atomic::AtomicBool;
    static SAID: AtomicBool = AtomicBool::new(false);
    if SAID.swap(true, Ordering::Relaxed) {
        return;
    }
    let _ = (kind, at, identity);
    const MESSAGE: &[u8] = b"rosemary: pthread: a copied initialised pthread object was adopted\n";
    // SAFETY: a fixed static buffer; `write` is async-signal-safe.
    unsafe { libc::write(libc::STDERR_FILENO, MESSAGE.as_ptr().cast(), MESSAGE.len()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sizes are Darwin's, checked against the host's own types where the two are
    /// the same types. This is the assertion that makes the layout measured rather
    /// than believed - and it can only be made on a Mac.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_sizes_are_the_ones_this_host_declares() {
        assert_eq!(size::ATTR, size_of::<libc::pthread_attr_t>());
        assert_eq!(size::MUTEX, size_of::<libc::pthread_mutex_t>());
        assert_eq!(size::MUTEXATTR, size_of::<libc::pthread_mutexattr_t>());
        assert_eq!(size::COND, size_of::<libc::pthread_cond_t>());
        assert_eq!(size::CONDATTR, size_of::<libc::pthread_condattr_t>());
        assert_eq!(size::RWLOCK, size_of::<libc::pthread_rwlock_t>());
        assert_eq!(size::ONCE, size_of::<libc::pthread_once_t>());
    }

    /// And the signatures are the ones the host's own static initializers write,
    /// which is the other half of the same cross-check: these values are how a
    /// `PTHREAD_MUTEX_INITIALIZER` in a guest binary is recognised.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_static_initializers_write_the_signatures_this_module_looks_for() {
        let m = libc::PTHREAD_MUTEX_INITIALIZER;
        // SAFETY: reading the first eight bytes of a live `pthread_mutex_t`, which
        // the public struct declares as its `long __sig`.
        let first = unsafe { *(&m as *const libc::pthread_mutex_t as *const u64) };
        assert_eq!(first, sig::MUTEX_INIT);

        let c = libc::PTHREAD_COND_INITIALIZER;
        assert_eq!(
            unsafe { *(&c as *const libc::pthread_cond_t as *const u64) },
            sig::COND_INIT
        );
    }

    /// Every kind's lifecycle words and body must fit inside the object they live in.
    /// The body sizes each implementation needs are asserted where they are laid out.
    #[test]
    fn every_record_fits_the_object_it_lives_in() {
        for (kind, bytes) in [
            (Kind::Mutex, size::MUTEX),
            (Kind::Cond, size::COND),
            (Kind::RwLock, size::RWLOCK),
            (Kind::Once, size::ONCE),
            (Kind::Attr, size::ATTR),
            (Kind::MutexAttr, size::MUTEXATTR),
            (Kind::CondAttr, size::CONDATTR),
            (Kind::RwLockAttr, size::RWLOCKATTR),
        ] {
            let needed = kind.body() as usize + 8;
            assert!(
                bytes >= needed,
                "{kind:?} needs {needed} bytes and has {bytes}"
            );
        }
    }

    /// No signature this crate writes can be mistaken for one Darwin writes, in
    /// either direction - which is what lets a single word distinguish "the guest
    /// statically initialised this" from "we have already built it".
    #[test]
    fn our_signatures_cannot_collide_with_darwins() {
        let darwin = [
            sig::MUTEX_INIT,
            sig::ERRORCHECK_MUTEX_INIT,
            sig::RECURSIVE_MUTEX_INIT,
            sig::FIRSTFIT_MUTEX_INIT,
            sig::COND_INIT,
            sig::ONCE_INIT,
            sig::RWLOCK_INIT,
        ];
        let mine: Vec<u64> = [
            Kind::Mutex,
            Kind::Cond,
            Kind::RwLock,
            Kind::Once,
            Kind::Attr,
            Kind::MutexAttr,
            Kind::CondAttr,
            Kind::RwLockAttr,
        ]
        .iter()
        .map(|k| ours(*k))
        .chain([BUSY, DEAD])
        .collect();

        for d in darwin {
            assert!(d < 1 << 32, "a Darwin signature fits in 32 bits");
            assert!(!mine.contains(&d), "{d:#x} is claimed by both");
        }
        let mut sorted = mine.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), mine.len(), "two kinds share a signature");
    }
}
