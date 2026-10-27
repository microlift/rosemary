//! Thread-local variables, the way dyld provides them.
//!
//! A Mach-O reads a `_Thread_local` or C++ `thread_local` by calling through its
//! `tlv_descriptor`, `{thunk, key, offset}`, with the descriptor in the first
//! argument register. dyld gives each image one pthread key, rewrites every
//! descriptor of the image to `{tlv_get_addr, key, offset}`, and `tlv_get_addr`
//! answers from the thread pointer: `tsd[key] + offset`, with a slow path only for a
//! thread's first touch of the image. This module is that, piece for piece:
//!
//! * [`registry`]: one Darwin key per image (see [`darwin::tls::key`]) and the
//!   block's layout, written once at [`register`].
//! * [`thunk`]: `tlv_get_addr` - x86-64 guest code for an emulated guest, so a hit
//!   never leaves the JIT, and the host's own for a native one.
//! * [`block`]: the slow path, which allocates the block, sets the key and runs the
//!   image's `__thread_init` constructors.
//! * [`state`]: `_tlv_atexit`, whose destructors run at thread exit before any
//!   block is freed - dyld's `tlv_finalize`.

mod block;
mod registry;
mod state;
mod thunk;

use machish::report::LinkReport;

use crate::exec::Engine;

pub use block::tlv_allocate;
pub use state::tlv_atexit;
pub use thunk::guest::{contains, import_at, import_site, lift_region, locate};
pub use thunk::tlv_bootstrap_thunk;

/// Make every thread-local in `report` addressable to code running under `engine`,
/// as dyld's `tlv_initialize_descriptors` does: a key per image, and every
/// descriptor rewritten to point at `tlv_get_addr` with that key.
///
/// A descriptor whose offset is outside its image's block is left bound to
/// `__tlv_bootstrap`, which names it rather than answering.
pub fn register(report: &LinkReport, engine: Engine) -> usize {
    // Older than every image's key, so its destructor runs first. See [`state`].
    let _ = state::key();
    let thunk = thunk::entry(engine);
    let mut registered = 0;
    for image in &report.images {
        let Some(t) = registry::layout(image) else {
            continue;
        };
        let key = match registry::add(image, t) {
            Ok(key) => key,
            Err(rc) => {
                eprintln!(
                    "rosemary: {}: no key for its thread-locals (errno {rc})",
                    image.name
                );
                continue;
            }
        };
        for d in image
            .thread_locals
            .iter()
            .filter(|d| d.offset < t.total_size)
        {
            // SAFETY: `d.at` is a descriptor in the image's own `__thread_vars`,
            // which is writable data - the binder has just written its thunk - and
            // guest addresses are host addresses.
            unsafe {
                let words = d.at as usize as *mut u64;
                words.write(thunk);
                words.add(1).write(u64::from(key.index()));
            }
            registered += 1;
        }
    }
    registered
}

/// Run the calling thread's `_tlv_atexit` terminators, as dyld's `_tlv_exit` does
/// when the process exits.
pub fn exit() {
    state::exit();
}

/// Darwin's `void exit(int)`: the calling thread's thread-local terminators first,
/// then the host's `exit` and with it every `atexit` handler.
pub extern "C" fn darwin_exit(status: i32) -> ! {
    exit();
    // SAFETY: ends the process; nothing here is borrowed across it.
    unsafe { libc::exit(status) }
}

/// Whether `symbol` names dyld's bootstrap, which [`register`] replaces in every
/// descriptor it accepts - so a trampoline for it would never be reached.
pub fn is_bootstrap(symbol: &str) -> bool {
    machish::extensions::tlv::BOOTSTRAP_SYMBOLS.contains(&symbol)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// A descriptor as `register` leaves one, for a block `register_image` made.
    fn descriptor(key: darwin::tls::key::Key, offset: u64) -> &'static [u64; 3] {
        Box::leak(Box::new([0, u64::from(key.index()), offset]))
    }

    fn address(d: &[u64; 3]) -> *mut u8 {
        tlv_allocate(d.as_ptr() as *const u8)
    }

    fn image(initial: &'static [u8], total: usize, init_funcs: &[u64]) -> darwin::tls::key::Key {
        registry::add_for_test(
            initial.as_ptr() as usize as u64,
            initial.len(),
            total,
            init_funcs,
        )
    }

    #[test]
    fn an_unregistered_descriptor_answers_null_rather_than_guessing() {
        let unknown: &[u64; 3] = &[0, u64::from(u32::MAX), 0];
        assert!(address(unknown).is_null());
        assert!(tlv_allocate(std::ptr::null()).is_null());
        assert!(block::tlv_bootstrap(unknown.as_ptr() as *const u8).is_null());
    }

    #[test]
    fn the_block_is_the_key_s_value_and_per_thread() {
        let key = image(&[], 64, &[]);
        let d = descriptor(key, 8);

        let mine = address(d);
        assert!(!mine.is_null(), "a registered descriptor has a block");
        assert_eq!(address(d), mine, "a second touch is the same address");
        assert_eq!(
            key.get() as usize + 8,
            mine as usize,
            "the fast path's answer: the key's value plus the offset"
        );
        // SAFETY: eight bytes into this thread's 64-byte block.
        unsafe { *mine = 0x5e };

        let theirs = std::thread::spawn(move || {
            let theirs = address(d);
            // SAFETY: as above, on the spawned thread's own block.
            assert_eq!(unsafe { *theirs }, 0, "a fresh block is zeroed");
            theirs as usize
        })
        .join()
        .unwrap();
        assert_ne!(theirs, mine as usize, "a second thread gets its own block");
    }

    #[test]
    fn initial_values_are_copied_per_thread_and_never_aliased() {
        static IMAGE: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let key = image(&IMAGE, 32, &[]);
        let (data, bss) = (descriptor(key, 0), descriptor(key, 24));

        let mine = address(data);
        let tail = address(bss);
        // SAFETY: both point into this thread's 32-byte block.
        unsafe {
            assert_eq!(std::slice::from_raw_parts(mine, IMAGE.len()), &IMAGE);
            assert_eq!(*tail, 0, "past __thread_data a fresh block is zero");
            mine.write(0xff);
        }
        assert_eq!(IMAGE[0], 1, "a copy, not an alias");

        std::thread::spawn(move || {
            // SAFETY: the spawned thread's own block.
            unsafe { assert_eq!(*address(data), 1, "not the other thread's write") };
        })
        .join()
        .unwrap();
    }

    #[test]
    fn constructors_run_once_per_thread_in_reverse_order_with_the_key_already_set() {
        static ORDER: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        let key = image(&[], 64, &[1, 2]);
        let (d, other) = (descriptor(key, 0), descriptor(key, 8));
        let run = |func| {
            if func == 2 {
                // A constructor reading a sibling variable: the key is set, so this
                // is the block being built, and nothing is constructed twice.
                let nested = block::address_with(other.as_ptr() as *const u8, |_| {
                    panic!("constructor ran twice")
                })?;
                assert_eq!(nested as usize, key.get() as usize + 8);
            }
            ORDER.lock().unwrap().push(func as u8);
            Ok(())
        };

        let first = block::address_with(d.as_ptr() as *const u8, run).unwrap();
        assert!(!first.is_null());
        assert_eq!(*ORDER.lock().unwrap(), [2, 1]);
        assert_eq!(
            block::address_with(d.as_ptr() as *const u8, run).unwrap(),
            first
        );
        assert_eq!(*ORDER.lock().unwrap(), [2, 1]);

        std::thread::spawn(move || {
            block::address_with(d.as_ptr() as *const u8, |func| {
                ORDER.lock().unwrap().push(func as u8);
                Ok(())
            })
            .unwrap();
        })
        .join()
        .unwrap();
        assert_eq!(*ORDER.lock().unwrap(), [2, 1, 2, 1]);
    }

    #[test]
    fn a_failed_constructor_is_not_retried_or_exposed() {
        let key = image(&[], 64, &[1, 2, 3]);
        let (d, other) = (descriptor(key, 0), descriptor(key, 8));
        std::thread::spawn(move || {
            let mut called = Vec::new();
            let result = block::address_with(d.as_ptr() as *const u8, |func| {
                called.push(func);
                if func == 2 {
                    Err("failed".to_string())
                } else {
                    Ok(())
                }
            });
            assert_eq!(result.unwrap_err(), "failed");
            assert_eq!(called, [3, 2]);
            assert!(
                key.get().is_null(),
                "the half-built block is not left in the key"
            );
            let again = |_| panic!("retried");
            assert!(
                block::address_with(d.as_ptr() as *const u8, again)
                    .unwrap()
                    .is_null()
            );
            assert!(
                block::address_with(other.as_ptr() as *const u8, again)
                    .unwrap()
                    .is_null()
            );
        })
        .join()
        .unwrap();
    }

    #[test]
    fn terminators_run_last_first_including_ones_registered_while_running() {
        static ORDER: Mutex<Vec<u64>> = Mutex::new(Vec::new());
        unsafe extern "C" fn record(object: *mut core::ffi::c_void) {
            let n = object as u64;
            ORDER.lock().unwrap().push(n);
            if n == 1 {
                // SAFETY: `record` takes any object.
                unsafe { tlv_atexit(Some(record), 9 as *mut _) };
            }
        }
        std::thread::spawn(|| {
            // SAFETY: as above.
            unsafe {
                tlv_atexit(Some(record), std::ptr::dangling_mut());
                tlv_atexit(Some(record), 2 as *mut _);
                tlv_atexit(Some(record), 3 as *mut _);
            }
        })
        .join()
        .unwrap();
        assert_eq!(*ORDER.lock().unwrap(), [3, 2, 1, 9]);
    }

    #[test]
    fn a_terminator_still_reads_its_thread_s_block() {
        static D: AtomicU64 = AtomicU64::new(0);
        static READ: AtomicU64 = AtomicU64::new(0);
        unsafe extern "C" fn read(_: *mut core::ffi::c_void) {
            let d = D.load(Ordering::SeqCst) as usize as *const u8;
            // SAFETY: the variable this thread wrote before it ended.
            READ.store(u64::from(unsafe { *tlv_allocate(d) }), Ordering::SeqCst);
        }
        // Registered after the state key exists, as `register` arranges.
        let _ = state::key();
        let key = image(&[], 16, &[]);
        D.store(
            descriptor(key, 0).as_ptr() as usize as u64,
            Ordering::SeqCst,
        );
        std::thread::spawn(|| {
            let d = D.load(Ordering::SeqCst) as usize as *const u8;
            // SAFETY: this thread's own block.
            unsafe { *tlv_allocate(d) = 0x42 };
            // SAFETY: `read` takes any object.
            unsafe { tlv_atexit(Some(read), std::ptr::null_mut()) };
        })
        .join()
        .unwrap();
        assert_eq!(
            READ.load(Ordering::SeqCst),
            0x42,
            "terminators run before the blocks they read are freed"
        );
    }

    /// A read from a later pthread destructor - after the block was freed - gets a
    /// fresh block, as under dyld, rather than null.
    #[test]
    fn a_read_after_teardown_freed_the_block_gets_a_fresh_one() {
        static D: AtomicU64 = AtomicU64::new(0);
        static ANSWERED: AtomicU64 = AtomicU64::new(0);
        let key = image(&[], 64, &[]);
        D.store(
            descriptor(key, 0).as_ptr() as usize as u64,
            Ordering::SeqCst,
        );

        unsafe extern "C" fn late(_: *mut core::ffi::c_void) {
            let address = tlv_allocate(D.load(Ordering::SeqCst) as usize as *const u8);
            ANSWERED.store(address as usize as u64, Ordering::SeqCst);
        }
        // Newer than the image's key, so it runs after the block was freed.
        let late_key = darwin::tls::key::Key::create(Some(late)).unwrap();
        std::thread::spawn(move || {
            assert!(!tlv_allocate(D.load(Ordering::SeqCst) as usize as *const u8).is_null());
            late_key.set(std::ptr::dangling_mut());
        })
        .join()
        .unwrap();
        assert_ne!(ANSWERED.load(Ordering::SeqCst), 0, "served, not refused");
    }

    #[test]
    fn the_bootstrap_is_published_as_the_register_preserving_thunk() {
        use machish::fixups::SymbolRef;
        use machish::resolve::SymbolProvider;
        let provider = crate::shims::runtime::provider();
        for name in machish::extensions::tlv::BOOTSTRAP_SYMBOLS {
            let symbol = SymbolRef {
                name,
                library: machish::fixups::LibraryRef::Flat,
                weak_import: false,
            };
            let resolved = provider
                .resolve(&symbol)
                .unwrap_or_else(|| panic!("{name} is not published at all"));
            assert_eq!(
                resolved.addr, tlv_bootstrap_thunk as *const () as usize as u64,
                "{name} must be bound to the thunk, not to the plain function"
            );
        }
    }
}
