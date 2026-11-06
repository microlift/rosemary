use std::ffi::c_void;
use std::hint::spin_loop;
use std::sync::atomic::{AtomicI32, AtomicI64, AtomicPtr, Ordering, fence};

#[inline]
unsafe fn atomic_i32<'a>(ptr: *mut c_void) -> &'a AtomicI32 {
    // Darwin requires these values to be naturally aligned.
    //
    // SAFETY:
    // - caller guarantees `ptr` points to a valid writable int32_t
    // - OSAtomic requires natural alignment
    // - accesses to this storage must obey atomic-access rules
    unsafe { AtomicI32::from_ptr(ptr.cast::<i32>()) }
}

#[inline]
unsafe fn atomic_i64<'a>(ptr: *mut c_void) -> &'a AtomicI64 {
    // SAFETY: same requirements as above, for int64_t.
    unsafe { AtomicI64::from_ptr(ptr.cast::<i64>()) }
}

#[inline]
unsafe fn atomic_ptr<'a>(ptr: *mut c_void) -> &'a AtomicPtr<c_void> {
    // `ptr` is the address of the guest's pointer-sized storage, i.e.
    // void * volatile * from the Darwin API's perspective.
    //
    // SAFETY:
    // - caller guarantees suitably aligned, writable pointer storage
    // - all conflicting accesses obey atomic-access rules
    unsafe { AtomicPtr::from_ptr(ptr.cast::<*mut c_void>()) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicAdd32(amount: i32, value: *mut c_void) -> i32 {
    // fetch_add returns the old value; Darwin returns the new value. SeqCst
    // even without `Barrier`: on x86_64 Darwin every one of these is a `lock`
    // instruction, a full fence guests do rely on.
    unsafe { atomic_i32(value) }
        .fetch_add(amount, Ordering::SeqCst)
        .wrapping_add(amount)
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicAdd32Barrier(amount: i32, value: *mut c_void) -> i32 {
    unsafe { atomic_i32(value) }
        .fetch_add(amount, Ordering::SeqCst)
        .wrapping_add(amount)
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicAdd64(amount: i64, value: *mut c_void) -> i64 {
    unsafe { atomic_i64(value) }
        .fetch_add(amount, Ordering::SeqCst)
        .wrapping_add(amount)
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicAdd64Barrier(amount: i64, value: *mut c_void) -> i64 {
    unsafe { atomic_i64(value) }
        .fetch_add(amount, Ordering::SeqCst)
        .wrapping_add(amount)
}

// The increment and decrement pairs are real exports since 10.10, where the SDK
// stopped inlining them as `OSAtomicAdd*(+-1, ...)`; same result convention.

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicIncrement32(value: *mut c_void) -> i32 {
    unsafe { OSAtomicAdd32(1, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicIncrement32Barrier(value: *mut c_void) -> i32 {
    unsafe { OSAtomicAdd32Barrier(1, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicDecrement32(value: *mut c_void) -> i32 {
    unsafe { OSAtomicAdd32(-1, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicDecrement32Barrier(value: *mut c_void) -> i32 {
    unsafe { OSAtomicAdd32Barrier(-1, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicIncrement64(value: *mut c_void) -> i64 {
    unsafe { OSAtomicAdd64(1, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicIncrement64Barrier(value: *mut c_void) -> i64 {
    unsafe { OSAtomicAdd64Barrier(1, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicDecrement64(value: *mut c_void) -> i64 {
    unsafe { OSAtomicAdd64(-1, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicDecrement64Barrier(value: *mut c_void) -> i64 {
    unsafe { OSAtomicAdd64Barrier(-1, value) }
}

#[inline]
unsafe fn compare_and_swap32(old: i32, new: i32, value: *mut c_void) -> bool {
    unsafe { atomic_i32(value) }
        // A full fence with or without `Barrier`; see `OSAtomicAdd32`.
        .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

#[inline]
unsafe fn compare_and_swap64(old: i64, new: i64, value: *mut c_void) -> bool {
    unsafe { atomic_i64(value) }
        // A full fence with or without `Barrier`; see `OSAtomicAdd32`.
        .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicCompareAndSwap32(old: i32, new: i32, value: *mut c_void) -> bool {
    unsafe { compare_and_swap32(old, new, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicCompareAndSwap32Barrier(
    old: i32,
    new: i32,
    value: *mut c_void,
) -> bool {
    unsafe { compare_and_swap32(old, new, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicCompareAndSwap64(old: i64, new: i64, value: *mut c_void) -> bool {
    unsafe { compare_and_swap64(old, new, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicCompareAndSwap64Barrier(
    old: i64,
    new: i64,
    value: *mut c_void,
) -> bool {
    unsafe { compare_and_swap64(old, new, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicCompareAndSwapInt(old: i32, new: i32, value: *mut c_void) -> bool {
    unsafe { OSAtomicCompareAndSwap32(old, new, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicCompareAndSwapIntBarrier(
    old: i32,
    new: i32,
    value: *mut c_void,
) -> bool {
    unsafe { OSAtomicCompareAndSwap32Barrier(old, new, value) }
}

#[inline]
unsafe fn compare_and_swap_ptr(old: *mut c_void, new: *mut c_void, value: *mut c_void) -> bool {
    unsafe { atomic_ptr(value) }
        // A full fence with or without `Barrier`; see `OSAtomicAdd32`.
        .compare_exchange(old, new, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicCompareAndSwapPtr(
    old: *mut c_void,
    new: *mut c_void,
    value: *mut c_void,
) -> bool {
    unsafe { compare_and_swap_ptr(old, new, value) }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSAtomicCompareAndSwapPtrBarrier(
    old: *mut c_void,
    new: *mut c_void,
    value: *mut c_void,
) -> bool {
    unsafe { compare_and_swap_ptr(old, new, value) }
}

#[allow(non_snake_case)]
pub extern "C" fn OSMemoryBarrier() {
    fence(Ordering::SeqCst);
}

/// OSSpinLock
///
/// Darwin defines OSSpinLock as an int32_t.
///
/// 0       => unlocked
/// nonzero => locked
///
/// Apple explicitly documents only the zero/nonzero distinction, so using 1 as
/// the locked representation is sufficient.
#[allow(non_snake_case)]
pub unsafe extern "C" fn OSSpinLockTry(lock: *mut c_void) -> bool {
    unsafe { atomic_i32(lock) }
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSSpinLockLock(lock: *mut c_void) {
    let lock = unsafe { atomic_i32(lock) };

    // Do a normal CAS first. After contention is observed, spin mostly on
    // relaxed loads so we don't continuously issue expensive cache-line
    // ownership requests.
    loop {
        if lock
            .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }

        while lock.load(Ordering::Relaxed) != 0 {
            spin_loop();
        }
    }
}

#[allow(non_snake_case)]
pub unsafe extern "C" fn OSSpinLockUnlock(lock: *mut c_void) {
    unsafe { atomic_i32(lock) }.store(0, Ordering::Release);
}

exports::exports! {
    functions {
        "OSAtomicAdd32" => OSAtomicAdd32,
        "OSAtomicAdd32Barrier" => OSAtomicAdd32Barrier,
        "OSAtomicAdd64" => OSAtomicAdd64,
        "OSAtomicAdd64Barrier" => OSAtomicAdd64Barrier,
        "OSAtomicCompareAndSwap32" => OSAtomicCompareAndSwap32,
        "OSAtomicCompareAndSwap32Barrier" => OSAtomicCompareAndSwap32Barrier,
        "OSAtomicCompareAndSwap64" => OSAtomicCompareAndSwap64,
        "OSAtomicCompareAndSwap64Barrier" => OSAtomicCompareAndSwap64Barrier,
        "OSAtomicCompareAndSwapInt" => OSAtomicCompareAndSwapInt,
        "OSAtomicCompareAndSwapIntBarrier" => OSAtomicCompareAndSwapIntBarrier,
        "OSAtomicCompareAndSwapPtr" => OSAtomicCompareAndSwapPtr,
        "OSAtomicCompareAndSwapPtrBarrier" => OSAtomicCompareAndSwapPtrBarrier,
        "OSAtomicDecrement32" => OSAtomicDecrement32,
        "OSAtomicDecrement32Barrier" => OSAtomicDecrement32Barrier,
        "OSAtomicDecrement64" => OSAtomicDecrement64,
        "OSAtomicDecrement64Barrier" => OSAtomicDecrement64Barrier,
        "OSAtomicIncrement32" => OSAtomicIncrement32,
        "OSAtomicIncrement32Barrier" => OSAtomicIncrement32Barrier,
        "OSAtomicIncrement64" => OSAtomicIncrement64,
        "OSAtomicIncrement64Barrier" => OSAtomicIncrement64Barrier,
        "OSMemoryBarrier" => OSMemoryBarrier,
        "OSSpinLockLock" => OSSpinLockLock,
        "OSSpinLockTry" => OSSpinLockTry,
        "OSSpinLockUnlock" => OSSpinLockUnlock,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn increment_and_decrement_return_the_new_value() {
        let mut word: i32 = 41;
        let p = (&mut word as *mut i32).cast::<c_void>();
        // SAFETY: a live, aligned int32_t owned by this test.
        unsafe {
            assert_eq!(OSAtomicIncrement32(p), 42);
            assert_eq!(OSAtomicIncrement32Barrier(p), 43);
            assert_eq!(OSAtomicDecrement32(p), 42);
            assert_eq!(OSAtomicDecrement32Barrier(p), 41);
        }
        assert_eq!(word, 41);
    }

    #[test]
    fn a_32_bit_increment_wraps_like_the_c_type() {
        let mut word = i32::MAX;
        // SAFETY: a live, aligned int32_t owned by this test.
        let new = unsafe { OSAtomicIncrement32((&mut word as *mut i32).cast()) };
        assert_eq!(new, i32::MIN);
    }

    #[test]
    fn the_64_bit_pair_touches_all_eight_bytes() {
        let mut word: i64 = 1 << 32;
        let p = (&mut word as *mut i64).cast::<c_void>();
        // SAFETY: a live, aligned int64_t owned by this test.
        unsafe {
            assert_eq!(OSAtomicDecrement64(p), (1 << 32) - 1);
            assert_eq!(OSAtomicIncrement64Barrier(p), 1 << 32);
            assert_eq!(OSAtomicIncrement64(p), (1 << 32) + 1);
            assert_eq!(OSAtomicDecrement64Barrier(p), 1 << 32);
        }
    }

    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    /// Guest memory as the tests share it between threads.
    struct Word<T>(std::cell::UnsafeCell<T>);
    unsafe impl<T> Sync for Word<T> {}
    unsafe impl<T> Send for Word<T> {}
    impl<T> Word<T> {
        fn ptr(&self) -> *mut c_void {
            self.0.get().cast()
        }
    }

    #[test]
    fn concurrent_adds_increments_and_swaps_lose_nothing() {
        const THREADS: i64 = 8;
        const EACH: i64 = 100_000;
        let w32 = Arc::new(Word(std::cell::UnsafeCell::new(0i32)));
        let w64 = Arc::new(Word(std::cell::UnsafeCell::new(0i64)));
        let cas = Arc::new(Word(std::cell::UnsafeCell::new(0i32)));
        let threads: Vec<_> = (0..THREADS)
            .map(|_| {
                let (w32, w64, cas) = (Arc::clone(&w32), Arc::clone(&w64), Arc::clone(&cas));
                std::thread::spawn(move || unsafe {
                    for i in 0..EACH {
                        match i % 3 {
                            0 => OSAtomicIncrement32(w32.ptr()),
                            1 => OSAtomicAdd32(2, w32.ptr()),
                            _ => OSAtomicDecrement32(w32.ptr()),
                        };
                        OSAtomicAdd64(1 << 33, w64.ptr());
                        loop {
                            let seen = *(cas.ptr() as *const i32);
                            if OSAtomicCompareAndSwap32(seen, seen + 1, cas.ptr()) {
                                break;
                            }
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let per_thread = (0..EACH).map(|i| [1, 2, -1][(i % 3) as usize]).sum::<i64>();
        unsafe {
            assert_eq!(*(w32.ptr() as *const i32) as i64, THREADS * per_thread);
            assert_eq!(*(w64.ptr() as *const i64), (THREADS * EACH) << 33);
            assert_eq!(*(cas.ptr() as *const i32) as i64, THREADS * EACH);
        }
    }

    /// A lock built from the non-barrier compare-and-swap protects plain
    /// memory, as guest code written for x86_64 Darwin expects.
    #[test]
    fn a_lock_built_on_plain_compare_and_swap_excludes() {
        let lock = Arc::new(Word(std::cell::UnsafeCell::new(0i32)));
        let counter = Arc::new(Word(std::cell::UnsafeCell::new(0u64)));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let (lock, counter) = (Arc::clone(&lock), Arc::clone(&counter));
                std::thread::spawn(move || unsafe {
                    for _ in 0..20_000 {
                        while !OSAtomicCompareAndSwap32(0, 1, lock.ptr()) {
                            spin_loop();
                        }
                        *counter.0.get() += 1;
                        assert!(OSAtomicCompareAndSwap32(1, 0, lock.ptr()));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(unsafe { *counter.0.get() }, 6 * 20_000);
    }

    /// Store, then a non-barrier add, then load: the add is a full fence on
    /// x86_64 Darwin, so two threads doing this can never both read zero.
    #[test]
    fn a_plain_add_orders_a_store_before_a_later_load() {
        const ROUNDS: u32 = 50_000;
        let x = Arc::new(AtomicU32::new(0));
        let y = Arc::new(AtomicU32::new(0));
        let start = Arc::new(std::sync::Barrier::new(2));
        let side = |mine: Arc<AtomicU32>, theirs: Arc<AtomicU32>| {
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                let mut scratch = 0i32;
                let mut seen = Vec::with_capacity(ROUNDS as usize);
                for round in 0..ROUNDS {
                    start.wait();
                    mine.store(round + 1, Ordering::Relaxed);
                    unsafe { OSAtomicAdd32(0, (&mut scratch as *mut i32).cast()) };
                    seen.push(theirs.load(Ordering::Relaxed));
                    start.wait();
                }
                seen
            })
        };
        let a = side(Arc::clone(&x), Arc::clone(&y));
        let b = side(Arc::clone(&y), Arc::clone(&x));
        let (ra, rb) = (a.join().unwrap(), b.join().unwrap());
        for round in 0..ROUNDS as usize {
            let stale = |seen: u32| seen <= round as u32;
            assert!(
                !(stale(ra[round]) && stale(rb[round])),
                "round {round}: both loads passed the other's store"
            );
        }
    }
}
