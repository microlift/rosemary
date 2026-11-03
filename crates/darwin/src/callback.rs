use core::ffi::{c_int, c_void};

type Compare = unsafe extern "C" fn(*const c_void, *const c_void) -> c_int;

/// `void qsort(void *base, size_t nel, size_t width, int (*compar)(const void *, const void *))`
///
/// # Safety
///
/// `base` is `nel` elements of `width` bytes and `compar` is callable, which is
/// `qsort`'s own contract.
pub unsafe extern "C" fn qsort(
    base: *mut c_void,
    nel: usize,
    width: usize,
    compar: Option<Compare>,
) {
    // SAFETY: the caller's array and comparator, sorted by the host's own
    // `qsort` - which calls the comparator, so the boundary covers the sort.
    unsafe { crate::call_as_darwin!(libc::qsort(base, nel, width, compar)) }
}

/// `void *bsearch(const void *key, const void *base, size_t nel, size_t width,
/// int (*compar)(const void *, const void *))`
///
/// # Safety
///
/// As [`qsort`], with `key` readable.
pub unsafe extern "C" fn bsearch(
    key: *const c_void,
    base: *const c_void,
    nel: usize,
    width: usize,
    compar: Option<Compare>,
) -> *mut c_void {
    unsafe { crate::call_as_darwin!(libc::bsearch(key, base, nel, width, compar)) }
}

std::thread_local! {
    /// The block [`qsort_b`] on this thread is sorting with, which the host's
    /// `qsort` has no argument to carry.
    static SORT_BLOCK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The comparator [`qsort_b`] hands the host: its block's `invoke`, called
/// with the block first as the Block ABI has it.
unsafe extern "C" fn compare_with_block(a: *const c_void, b: *const c_void) -> c_int {
    let block = SORT_BLOCK.get();
    // SAFETY: `qsort_b`'s caller gave a live block; `invoke` is at a fixed
    // offset by the Block ABI, and is `int (*)(void *, const void *, const
    // void *)` for a comparator.
    unsafe {
        let invoke = ((block + crate::dispatch::block::INVOKE_OFFSET) as *const u64).read();
        let invoke = core::mem::transmute::<
            u64,
            unsafe extern "C" fn(u64, *const c_void, *const c_void) -> c_int,
        >(invoke);
        crate::call_as_darwin!(invoke(block, a, b))
    }
}

/// `void qsort_b(void *base, size_t nel, size_t width, int (^compar)(const
/// void *, const void *))`
///
/// # Safety
///
/// As [`qsort`], with `compar` a live block taking two elements.
pub unsafe extern "C" fn qsort_b(
    base: *mut c_void,
    nel: usize,
    width: usize,
    compar: *const c_void,
) {
    // A comparator may sort something of its own.
    let outer = SORT_BLOCK.replace(compar as u64);
    // SAFETY: the caller's; the comparator reads the block set above.
    unsafe { qsort(base, nel, width, Some(compare_with_block)) };
    SORT_BLOCK.set(outer);
}

exports::exports! {
    functions {
        "qsort" => qsort,
        "qsort_b" => qsort_b,
        "bsearch" => bsearch,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block literal with no captures, as a global block is laid out.
    #[repr(C)]
    struct Literal {
        isa: u64,
        flags: u32,
        reserved: u32,
        invoke: unsafe extern "C" fn(u64, *const c_void, *const c_void) -> c_int,
        descriptor: u64,
    }

    unsafe extern "C" fn descending(_block: u64, a: *const c_void, b: *const c_void) -> c_int {
        // SAFETY: the test below sorts `i32`s.
        unsafe { *b.cast::<i32>() - *a.cast::<i32>() }
    }

    #[test]
    fn qsort_b_orders_by_the_block() {
        let block = Literal {
            isa: 0,
            flags: 0,
            reserved: 0,
            invoke: descending,
            descriptor: 0,
        };
        let mut values = [2i32, 7, 1, 5];
        // SAFETY: four `i32`s and a block comparing two of them.
        unsafe {
            qsort_b(
                values.as_mut_ptr().cast(),
                values.len(),
                size_of::<i32>(),
                (&raw const block).cast(),
            )
        };
        assert_eq!(values, [7, 5, 2, 1]);
    }

    /// What the wrappers exist for: the comparator is entered with the Darwin
    /// thread pointer even though the thread that called `qsort` had the host's.
    ///
    /// Run on a thread of this test's own, because the boundary is free - and so
    /// proves nothing - on a thread that is inside Darwin code already.
    #[test]
    fn a_comparator_runs_with_the_guests_thread_pointer() {
        static SAW_DARWIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

        unsafe extern "C" fn compare(a: *const c_void, b: *const c_void) -> c_int {
            if !crate::tls::entered() {
                SAW_DARWIN.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            // SAFETY: `qsort` below is sorting `i32`s.
            unsafe { *a.cast::<i32>() - *b.cast::<i32>() }
        }

        std::thread::spawn(|| {
            // The premise the wrappers rest on, asserted rather than assumed: a
            // thread this runtime started is not inside Darwin code.
            #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
            assert!(
                !crate::tls::entered(),
                "a fresh thread already had the guest's thread pointer"
            );

            let mut values = [5i32, 3, 9, 1];
            // SAFETY: four `i32`s and a comparator that reads two of them.
            unsafe {
                qsort(
                    values.as_mut_ptr().cast(),
                    values.len(),
                    size_of::<i32>(),
                    Some(compare),
                )
            };
            assert_eq!(values, [1, 3, 5, 9]);

            let key = 9i32;
            // SAFETY: as above, over the array just sorted.
            let found = unsafe {
                bsearch(
                    (&raw const key).cast(),
                    values.as_ptr().cast(),
                    values.len(),
                    size_of::<i32>(),
                    Some(compare),
                )
            };
            assert!(!found.is_null(), "the key is in the array");
        })
        .join()
        .expect("the sorting thread");

        assert!(
            SAW_DARWIN.load(std::sync::atomic::Ordering::Relaxed),
            "the comparator ran with the host's thread pointer"
        );
    }
}
