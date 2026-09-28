use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use machish::report::LinkReport;
use static_init::dynamic;

/// Largest thread block this will allocate.
const MAX_BLOCK: u64 = 64 * 1024 * 1024;

/// One image's block layout, as [`machish`] described it.
struct Template {
    /// Slid VA the initialised prefix is copied from.
    initial_va: u64,
    initial_size: usize,
    total_size: usize,
    align: usize,
    /// Constructor addresses in section order.
    init_funcs: Vec<u64>,
}

/// What one descriptor address means.
struct Slot {
    /// Index into [`IMAGES`].
    image: usize,
    /// Byte offset of this variable within that image's block.
    offset: u64,
}

/// Every registered image's layout. Append-only; indices are handed out as
/// `Slot::image` and must stay valid.
static IMAGES: RwLock<Vec<Template>> = RwLock::new(Vec::new());

/// Every known `tlv_descriptor`, by its own address.
static SLOTS: RwLock<BTreeMap<u64, Slot>> = RwLock::new(BTreeMap::new());

/// How many times the registry has changed.
///
/// Read on every access and written once per [`register`], so it is the cheapest shared
/// thing in this module.
static EPOCH: AtomicU64 = AtomicU64::new(0);

/// Entries in one thread's descriptor cache.
const CACHE: usize = 64;

/// Which cache line a descriptor address falls on.
fn line(at: u64) -> usize {
    (at.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as usize & (CACHE - 1)
}

/// One thread's view of thread-local storage.
struct ThreadTls {
    /// The [`EPOCH`] `cache` was filled under. A mismatch means some image was
    /// registered since, and every entry is discarded rather than trusted.
    epoch: u64,
    /// Direct-mapped descriptor address to this thread's address for it. A key of zero
    /// is an empty entry, which costs nothing.
    cache: [(u64, *mut u8); CACHE],
    /// This thread's blocks, one per image that has thread-locals, allocated on
    /// first touch and freed when the thread ends.
    blocks: BTreeMap<usize, Block>,
    /// How many teardown passes have reached this thread's storage - see [`discard`].
    passes: u32,
}

impl ThreadTls {
    const fn new() -> ThreadTls {
        ThreadTls {
            epoch: 0,
            cache: [(0, std::ptr::null_mut()); CACHE],
            blocks: BTreeMap::new(),
            passes: 0,
        }
    }
}

/// How many times libpthread calls a key's destructor before giving up.
///
/// `PTHREAD_DESTRUCTOR_ITERATIONS`, which is four on Darwin and on glibc alike. The
/// last of those calls is this runtime's last chance to free the blocks, so it takes
/// it rather than leaking one set per thread.
const DESTRUCTOR_PASSES: u32 = 4;

/// The key this thread's storage hangs off.
///
/// A raw pthread key rather than a `thread_local!`, and the difference is what a read
/// **after** teardown does. A destructed `thread_local!` refuses every later access for
/// the rest of the thread's life, and there is no answer to give the guest but null -
/// which its very next instruction dereferences. Such reads are ordinary rather than
/// exotic: a destructor is code like any other, and a guest `pthread_key_create`
/// destructor runs after every thread-local destructor this runtime registered has.
///
/// dyld does not have that failure mode, so this must not either. [`discard`] hands the
/// storage back to the key for as many passes as libpthread makes, which keeps the
/// blocks addressable for the whole of teardown instead of up to the first pass that
/// happens to reach them.
#[inline(always)]
fn key() -> libc::pthread_key_t {
    #[dynamic]
    static KEY: libc::pthread_key_t = {
        let mut key: libc::pthread_key_t = 0;
        // SAFETY: a live local; `discard` is an `extern "C"` function of this module.
        let rc = unsafe { libc::pthread_key_create(&mut key, Some(discard)) };
        assert_eq!(rc, 0, "a guest cannot run without thread-local storage");
        key
    };
    *KEY
}

/// This thread's storage, created on first touch. Null only if the key would not take
/// it, which leaves the caller with no storage to answer from.
fn storage() -> *mut ThreadTls {
    let key = key();
    // SAFETY: `key` came from `pthread_key_create` above and is never deleted; its
    // value on this thread is a `Box::into_raw` result of this function and nothing
    // else.
    let existing = unsafe { libc::pthread_getspecific(key) } as *mut ThreadTls;
    if !existing.is_null() {
        return existing;
    }
    let fresh = Box::into_raw(Box::new(ThreadTls::new()));
    // SAFETY: as above; the key takes ownership of `fresh` from here.
    if unsafe { libc::pthread_setspecific(key, fresh as *const core::ffi::c_void) } != 0 {
        // SAFETY: the key did not take it, so nothing else holds the allocation.
        drop(unsafe { Box::from_raw(fresh) });
        return std::ptr::null_mut();
    }
    fresh
}

/// Called by libpthread with this thread's storage when the thread ends.
///
/// Puts it straight back for every pass but the last, so a destructor that has not run
/// yet - the guest's own, or another of this runtime's - still reads the block it wrote
/// rather than a fresh zeroed one.
///
/// # Safety
///
/// Called with the value this thread stored, which is a [`storage`] allocation and
/// nothing else.
unsafe extern "C" fn discard(block: *mut core::ffi::c_void) {
    if block.is_null() {
        return;
    }
    let tls = block as *mut ThreadTls;
    // SAFETY: the caller's guarantee, on the thread that owns the allocation.
    let passes = unsafe {
        (*tls).passes += 1;
        (*tls).passes
    };
    // SAFETY: as `storage`; handing the same allocation back to the same key.
    if passes < DESTRUCTOR_PASSES
        && unsafe { libc::pthread_setspecific(key(), block as *const core::ffi::c_void) } == 0
    {
        return;
    }
    // SAFETY: libpthread will not call this again, and nothing else holds it.
    drop(unsafe { Box::from_raw(tls) });
}

/// One thread's block for one image.
struct Block {
    ptr: *mut u8,
    layout: Layout,
    state: InitState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InitState {
    Initializing,
    Ready,
    Failed,
}

impl Drop for Block {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `alloc_zeroed` with exactly this layout and is
        // owned by this `Block`, which is reachable only from the thread-local
        // map being torn down.
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

/// Make every thread-local in `report` addressable.
pub fn register(report: &LinkReport) -> usize {
    let mut registered = 0;
    for image in &report.images {
        let Some(t) = image.tlv_template else {
            continue;
        };
        if image.thread_locals.is_empty() || t.total_size == 0 || t.total_size > MAX_BLOCK {
            continue;
        }
        let template = Template {
            initial_va: t.initial_va,
            initial_size: (t.initial_size.min(t.total_size)) as usize,
            total_size: t.total_size as usize,
            align: 1usize << t.align.min(16),
            init_funcs: image.tlv_init_funcs.clone(),
        };

        let mut images = IMAGES.write().unwrap_or_else(|e| e.into_inner());
        images.push(template);
        let index = images.len() - 1;
        drop(images);

        let mut slots = SLOTS.write().unwrap_or_else(|e| e.into_inner());
        for d in &image.thread_locals {
            if d.offset >= t.total_size {
                // The descriptor indexes past the block the sections describe.
                continue;
            }
            slots.insert(
                d.at,
                Slot {
                    image: index,
                    offset: d.offset,
                },
            );
            registered += 1;
        }
    }
    // After the slots are visible, and released so that a thread which observes
    // the new epoch cannot then read the map without seeing them.
    EPOCH.fetch_add(1, Ordering::Release);
    registered
}

/// `void *__tlv_bootstrap(struct tlv_descriptor *)` - this thread's address for the
/// variable `descriptor` describes.
///
/// A real native arm64 function, which is the point:
///
/// Returns null for a descriptor nobody registered, which is the only answer available
/// inside a C ABI with no error channel - and it faults in the guest at the access
/// rather than corrupting anything.
///
/// The name is only half of what a guest calls. It is reached through
/// [`bootstrap_thunk`], and a guest's generated code relies on the rest: the
/// address of a thread-local is computed without spilling anything first, so
/// dyld's `_tlv_get_addr` preserves every register except the result and this
/// must too. A `SIGSEGV` one instruction after a working thread-local read is
/// what publishing the plain SysV function instead of the thunk looks like.
pub extern "C" fn bootstrap(descriptor: *const u8) -> *mut u8 {
    let at = descriptor as usize as u64;
    match bootstrap_with(at, call_guest_init) {
        Ok(address) => address,
        Err(error) => {
            eprintln!("rosetta3: __tlv_bootstrap({at:#x}): {error}");
            std::ptr::null_mut()
        }
    }
}

enum Resolution {
    Address(*mut u8),
    Initialize {
        image: usize,
        address: *mut u8,
        funcs: Vec<u64>,
    },
}

fn bootstrap_with(
    at: u64,
    mut call: impl FnMut(u64) -> Result<(), String>,
) -> Result<*mut u8, String> {
    let tls = storage();
    if tls.is_null() {
        say_without_storage(at);
        return Ok(std::ptr::null_mut());
    }
    // The mutable borrow ends before guest constructors run; they may re-enter here.
    let (image, address, funcs) = match unsafe { resolve(&mut *tls, at) } {
        Resolution::Address(address) => return Ok(address),
        Resolution::Initialize {
            image,
            address,
            funcs,
        } => (image, address, funcs),
    };

    let result = funcs.iter().rev().try_for_each(|&func| call(func));
    let thread = unsafe { &mut *tls };
    let block = thread
        .blocks
        .get_mut(&image)
        .expect("new TLV block remains registered");
    block.state = if result.is_ok() {
        InitState::Ready
    } else {
        InitState::Failed
    };
    if result.is_ok() {
        thread.cache[line(at)] = (at, address);
    }
    result.map(|()| address)
}

/// [`bootstrap`] behind the register discipline a `tlv_descriptor` thunk owes its
/// caller.
///
/// dyld's `_tlv_get_addr` saves x1-x16 and q0-q7 around its slow path and the
/// compiler counts on it
#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
pub unsafe extern "C" fn bootstrap_thunk(_descriptor: *const u8) -> *mut u8 {
    core::arch::naked_asm!(
        "stp x29, x30, [sp, #-16]!",
        "mov x29, sp",
        "stp x1, x2, [sp, #-16]!",
        "stp x3, x4, [sp, #-16]!",
        "stp x5, x6, [sp, #-16]!",
        "stp x7, x8, [sp, #-16]!",
        "stp x9, x10, [sp, #-16]!",
        "stp x11, x12, [sp, #-16]!",
        "stp x13, x14, [sp, #-16]!",
        "stp x15, x16, [sp, #-16]!",
        "stp q0, q1, [sp, #-32]!",
        "stp q2, q3, [sp, #-32]!",
        "stp q4, q5, [sp, #-32]!",
        "stp q6, q7, [sp, #-32]!",
        "bl {bootstrap}",
        "ldp q6, q7, [sp], #32",
        "ldp q4, q5, [sp], #32",
        "ldp q2, q3, [sp], #32",
        "ldp q0, q1, [sp], #32",
        "ldp x15, x16, [sp], #16",
        "ldp x13, x14, [sp], #16",
        "ldp x11, x12, [sp], #16",
        "ldp x9, x10, [sp], #16",
        "ldp x7, x8, [sp], #16",
        "ldp x5, x6, [sp], #16",
        "ldp x3, x4, [sp], #16",
        "ldp x1, x2, [sp], #16",
        "ldp x29, x30, [sp], #16",
        "ret",
        bootstrap = sym bootstrap,
    )
}

/// [`bootstrap`] behind the same discipline on x86-64.
///
/// dyld's `_tlv_get_addr` saves every caller-saved register except `rax` around its
/// slow path, and callers count on that here for the same reason they do on arm64.
/// `rax` is the return value, so it is the one register the thunk is free to lose.
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
pub unsafe extern "C" fn bootstrap_thunk(_descriptor: *const u8) -> *mut u8 {
    core::arch::naked_asm!(
        "push rbp",
        "mov rbp, rsp",
        // Eight pushes keep the sixteen-byte alignment the frame arrived with,
        // so the `sub` below lands the `call` on an aligned stack.
        "push rdi",
        "push rsi",
        "push rdx",
        "push rcx",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "sub rsp, 128",
        "movaps [rsp + 0x00], xmm0",
        "movaps [rsp + 0x10], xmm1",
        "movaps [rsp + 0x20], xmm2",
        "movaps [rsp + 0x30], xmm3",
        "movaps [rsp + 0x40], xmm4",
        "movaps [rsp + 0x50], xmm5",
        "movaps [rsp + 0x60], xmm6",
        "movaps [rsp + 0x70], xmm7",
        "call {bootstrap}",
        "movaps xmm0, [rsp + 0x00]",
        "movaps xmm1, [rsp + 0x10]",
        "movaps xmm2, [rsp + 0x20]",
        "movaps xmm3, [rsp + 0x30]",
        "movaps xmm4, [rsp + 0x40]",
        "movaps xmm5, [rsp + 0x50]",
        "movaps xmm6, [rsp + 0x60]",
        "movaps xmm7, [rsp + 0x70]",
        "add rsp, 128",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rcx",
        "pop rdx",
        "pop rsi",
        "pop rdi",
        "pop rbp",
        "ret",
        bootstrap = sym bootstrap,
    )
}

/// Find the address and identify a block that still needs construction.
fn resolve(tls: &mut ThreadTls, at: u64) -> Resolution {
    // Acquire pairs with the release in `register`: observing the new epoch
    // means observing the slots that came with it.
    let epoch = EPOCH.load(Ordering::Acquire);
    if tls.epoch != epoch {
        // A program was registered since these were resolved.
        tls.cache = [(0, std::ptr::null_mut()); CACHE];
        tls.epoch = epoch;
    }

    let line = line(at);
    if at != 0 && tls.cache[line].0 == at {
        return Resolution::Address(tls.cache[line].1);
    }

    let (image, offset) = {
        let slots = SLOTS.read().unwrap_or_else(|e| e.into_inner());
        match slots.get(&at) {
            Some(slot) => (slot.image, slot.offset),
            None => {
                say_unregistered(at);
                return Resolution::Address(std::ptr::null_mut());
            }
        }
    };

    let mut init_funcs = None;
    if let std::collections::btree_map::Entry::Vacant(slot) = tls.blocks.entry(image) {
        let Some(mut fresh) = allocate(image) else {
            return Resolution::Address(std::ptr::null_mut());
        };
        let funcs = init_funcs_of(image);
        if !funcs.is_empty() {
            fresh.state = InitState::Initializing;
            init_funcs = Some(funcs);
        }
        // A constructor may re-enter bootstrap for another variable in this block.
        slot.insert(fresh);
    }
    let block = &tls.blocks[&image];
    if block.state == InitState::Failed {
        return Resolution::Address(std::ptr::null_mut());
    }
    // SAFETY: `offset` was checked against `total_size` at registration and the
    // block was allocated with exactly that many bytes.
    let address = unsafe { block.ptr.add(offset as usize) };

    if let Some(funcs) = init_funcs {
        return Resolution::Initialize {
            image,
            address,
            funcs,
        };
    }
    if block.state == InitState::Ready {
        tls.cache[line] = (at, address);
    }
    Resolution::Address(address)
}

/// One image's `__thread_bss_init_func` list, copied out from under the lock.
fn init_funcs_of(image: usize) -> Vec<u64> {
    let images = IMAGES.read().unwrap_or_else(|e| e.into_inner());
    images
        .get(image)
        .map_or_else(Vec::new, |t| t.init_funcs.clone())
}

/// The active backend executes guest constructors with no arguments.
fn call_guest_init(at: u64) -> Result<(), String> {
    let exec = crate::exec::active().ok_or("no execution backend is active")?;
    exec.call(at, &[])
        .map(|_| ())
        .map_err(|error| format!("thread-local constructor at {at:#x}: {error}"))
}

/// Name a descriptor this runtime has no block for, once each.
///
/// Null is the only value a C ABI with no error channel can return, and the guest's
/// very next instruction dereferences it - so without this the failure arrives as a
/// segfault inside guest code with nothing anywhere naming a thread-local.
fn say_unregistered(at: u64) {
    static SAID: Mutex<Vec<u64>> = Mutex::new(Vec::new());
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if said.contains(&at) {
        return;
    }
    said.push(at);
    eprintln!(
        "rosetta3: __tlv_bootstrap({at:#x}): no thread-local registered at that descriptor \
         - the guest will read address zero"
    );
}

/// Report a thread that could not be given storage at all, once for the process.
///
/// Only [`key`] refusing a value produces this - a thread past its last teardown pass,
/// or one libpthread has no room for. There is no block to answer from either way.
fn say_without_storage(at: u64) {
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| {
        eprintln!(
            "rosetta3: __tlv_bootstrap({at:#x}): this thread will not hold thread-local \
             storage - the guest will read address zero"
        );
    });
}

/// A zeroed block for `image`, with its initialised prefix copied in.
fn allocate(image: usize) -> Option<Block> {
    let images = IMAGES.read().unwrap_or_else(|e| e.into_inner());
    let t = images.get(image)?;
    let layout = Layout::from_size_align(t.total_size, t.align).ok()?;

    // SAFETY: the layout is non-zero-sized - `register` rejects `total_size` of
    // zero - and `alloc_zeroed` is the whole of the "and the rest is zero" half
    // of the block's contract.
    let ptr = unsafe { alloc_zeroed(layout) };
    if ptr.is_null() {
        return None;
    }

    if t.initial_size > 0 {
        // SAFETY: `initial_va` is a slid VA inside the guest's own mapped
        // `__thread_data`, which is live for as long as the program is loaded,
        // and the guest is identity-mapped so the address is the pointer.
        // `initial_size` was clamped to `total_size` at registration, so the
        // destination has room.
        unsafe {
            std::ptr::copy_nonoverlapping(t.initial_va as usize as *const u8, ptr, t.initial_size);
        }
    }
    Some(Block {
        ptr,
        layout,
        state: InitState::Ready,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unregistered_descriptor_answers_null_rather_than_guessing() {
        assert!(bootstrap(0xdead_beef as *const u8).is_null());
        assert!(bootstrap(std::ptr::null()).is_null());
    }

    /// The cache must not turn a miss into a hit for a *different* descriptor
    /// that happens to land on the same line, which is the one way a
    /// direct-mapped cache can hand back someone else's variable.
    #[test]
    fn a_colliding_descriptor_is_not_mistaken_for_the_cached_one() {
        // Two addresses on the same line, found rather than assumed: the mixing
        // constant decides which ones collide.
        let base = 0x1_0000u64;
        let (a, b) = (base..base + 0x10_000)
            .step_by(24)
            .find_map(|a| {
                (a + 24..a + 0x10_000)
                    .step_by(24)
                    .find(|&b| line(b) == line(a))
                    .map(|b| (a, b))
            })
            .expect("two of ten thousand addresses share one of sixty-four lines");

        let mut tls = ThreadTls {
            epoch: EPOCH.load(Ordering::Relaxed),
            ..ThreadTls::new()
        };
        let sentinel = 0x5eed_usize as *mut u8;
        tls.cache[line(a)] = (a, sentinel);

        assert!(matches!(resolve(&mut tls, a), Resolution::Address(p) if p == sentinel));
        assert!(matches!(resolve(&mut tls, b), Resolution::Address(p) if p.is_null()));
    }

    /// Two threads must not share a block, and a second access on one thread
    /// must return what the first did - which is the pair of properties the
    /// cache sits between, and the way it could break them is by being reached
    /// from more than the thread it belongs to.
    #[test]
    fn a_block_is_per_thread_and_a_repeat_access_is_the_same_address() {
        // Registered by hand rather than through a `LinkReport`, which needs an
        // image.
        let at = 0x3_0000u64;
        {
            let mut images = IMAGES.write().unwrap();
            images.push(Template {
                initial_va: 0,
                initial_size: 0,
                total_size: 64,
                align: 16,
                init_funcs: Vec::new(),
            });
            let image = images.len() - 1;
            drop(images);
            SLOTS.write().unwrap().insert(at, Slot { image, offset: 8 });
        }
        EPOCH.fetch_add(1, Ordering::Release);

        let mine = bootstrap(at as *const u8);
        assert!(!mine.is_null(), "a registered descriptor has a block");
        assert_eq!(
            bootstrap(at as *const u8),
            mine,
            "the cached answer must be the answer"
        );
        // SAFETY: `mine` is eight bytes into a sixty-four byte block owned by
        // this thread, so a byte write is in bounds and unshared.
        unsafe { *mine = 0x5e };

        let theirs = std::thread::spawn(move || {
            let theirs = bootstrap(at as *const u8);
            // SAFETY: as above, on the spawned thread's own block.
            assert_eq!(unsafe { *theirs }, 0, "a fresh block is zeroed");
            theirs as usize
        })
        .join()
        .expect("the thread reads its own thread-local");
        assert_ne!(
            theirs, mine as usize,
            "a second thread must get its own block, not this one's"
        );
    }

    /// A registration invalidates what threads resolved before it, because a
    /// descriptor address may now belong to a different program.
    #[test]
    fn a_new_epoch_discards_what_was_resolved_under_the_old_one() {
        let at = 0x2_0000u64;
        let mut tls = ThreadTls {
            epoch: EPOCH.load(Ordering::Relaxed),
            ..ThreadTls::new()
        };
        tls.cache[line(at)] = (at, 0x5eed_usize as *mut u8);

        EPOCH.fetch_add(1, Ordering::Release);
        assert!(matches!(resolve(&mut tls, at), Resolution::Address(p) if p.is_null()));
    }

    /// A thread-local read from a `pthread_key_create` destructor must still answer.
    ///
    /// This is the case a `thread_local!` cannot serve and the whole reason [`key`] is
    /// a raw one: libpthread runs key destructors after every thread-local's own, so a
    /// guest destructor touching a thread-local arrives once this runtime's storage
    /// would already be gone, and null is what the guest dereferences.
    #[test]
    fn a_read_from_a_thread_s_own_destructor_still_finds_its_block() {
        static AT: AtomicU64 = AtomicU64::new(0);
        static ANSWERED: AtomicU64 = AtomicU64::new(0);

        let at = 0x4_0000u64;
        {
            let mut images = IMAGES.write().unwrap();
            images.push(Template {
                initial_va: 0,
                initial_size: 0,
                total_size: 64,
                align: 16,
                init_funcs: Vec::new(),
            });
            let image = images.len() - 1;
            drop(images);
            SLOTS.write().unwrap().insert(at, Slot { image, offset: 0 });
        }
        EPOCH.fetch_add(1, Ordering::Release);
        AT.store(at, Ordering::SeqCst);
        // Here rather than on the spawned thread, so this module's key is the older of
        // the two and libpthread reaches it first in each pass - the order that broke.
        assert!(!bootstrap(at as *const u8).is_null());

        /// # Safety
        ///
        /// Called by libpthread with the value the thread below stored.
        unsafe extern "C" fn destructor(_: *mut core::ffi::c_void) {
            let address = bootstrap(AT.load(Ordering::SeqCst) as *const u8);
            ANSWERED.store(address as usize as u64, Ordering::SeqCst);
        }

        std::thread::spawn(move || {
            let mut key: libc::pthread_key_t = 0;
            // SAFETY: a live local, and a destructor of this module.
            assert_eq!(
                unsafe { libc::pthread_key_create(&mut key, Some(destructor)) },
                0
            );
            // Any non-null value: libpthread calls the destructor for a key that holds
            // one, and what it holds is never read here.
            // SAFETY: `key` is the one just created.
            assert_eq!(
                unsafe { libc::pthread_setspecific(key, &raw const AT as _) },
                0
            );
            assert!(!bootstrap(AT.load(Ordering::SeqCst) as *const u8).is_null());
        })
        .join()
        .expect("the thread reads its thread-local and then ends");

        assert_ne!(
            ANSWERED.load(Ordering::SeqCst),
            0,
            "a destructor's read must be served, not refused"
        );
    }

    /// A descriptor address no other test uses, so two tests registering in the
    /// same process cannot claim each other's variable.
    fn next_descriptor() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(0x9_0000);
        NEXT.fetch_add(0x1_0000, Ordering::Relaxed)
    }

    /// Register one image and one descriptor in it, the way `register` does
    /// from a `LinkReport`. Returns the descriptor's address.
    fn register_one(template: Template, offset: u64) -> u64 {
        let mut images = IMAGES.write().unwrap();
        images.push(template);
        let image = images.len() - 1;
        drop(images);
        register_slot(image, offset)
    }

    /// A second descriptor in an image [`register_one`] already registered.
    fn register_slot(image: usize, offset: u64) -> u64 {
        let at = next_descriptor();
        SLOTS.write().unwrap().insert(at, Slot { image, offset });
        EPOCH.fetch_add(1, Ordering::Release);
        at
    }

    #[test]
    fn initial_values_are_copied_per_thread_and_never_aliased() {
        static IMAGE: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let template = Template {
            initial_va: IMAGE.as_ptr() as usize as u64,
            initial_size: IMAGE.len(),
            total_size: 32,
            align: 16,
            init_funcs: Vec::new(),
        };
        let data = register_one(template, 0);
        let bss = {
            let image = SLOTS
                .read()
                .unwrap()
                .get(&data)
                .expect("just registered")
                .image;
            register_slot(image, 24)
        };

        let mine = bootstrap(data as *const u8);
        assert!(!mine.is_null(), "a registered descriptor has a block");
        let tail = bootstrap(bss as *const u8);
        // SAFETY: both pointers address this thread's 32-byte block.
        unsafe {
            assert_eq!(
                std::slice::from_raw_parts(mine, IMAGE.len()),
                &IMAGE,
                "the initialised prefix must arrive as the image's bytes"
            );
            assert_eq!(
                *tail, 0,
                "past __thread_data a fresh block is zero, whatever the image holds"
            );
            mine.write(0xff);
        }
        assert_eq!(
            IMAGE[0], 1,
            "the image's own bytes must not be the block: a copy, not an alias"
        );

        let theirs = std::thread::spawn(move || {
            let theirs = bootstrap(data as *const u8);
            let their_tail = bootstrap(bss as *const u8);
            // SAFETY: as above, on the spawned thread's own block.
            unsafe {
                assert_eq!(
                    *theirs, 1,
                    "a second thread must get the image's bytes, not this one's write"
                );
                assert_eq!(*their_tail, 0, "and a zero-filled tail");
            }
            theirs as usize
        })
        .join()
        .expect("the thread reads its own thread-local");
        assert_ne!(
            theirs, mine as usize,
            "a second thread must get its own block, not this one's"
        );
    }

    #[test]
    fn constructors_run_once_per_thread_in_reverse_order_and_allow_reentry() {
        static ORDER: Mutex<Vec<u8>> = Mutex::new(Vec::new());

        let at = register_one(
            Template {
                initial_va: 0,
                initial_size: 0,
                total_size: 64,
                align: 16,
                init_funcs: vec![1, 2],
            },
            0,
        );
        let image = SLOTS.read().unwrap()[&at].image;
        let other = register_slot(image, 8);
        let run = |func| {
            if func == 2 {
                let nested = bootstrap_with(other, |_| panic!("constructor ran twice"))?;
                assert!(!nested.is_null());
            }
            ORDER.lock().unwrap().push(func as u8);
            Ok(())
        };

        let first = bootstrap_with(at, run).unwrap();
        assert!(!first.is_null());
        assert_eq!(*ORDER.lock().unwrap(), [2, 1]);
        assert_eq!(bootstrap_with(at, run).unwrap(), first);
        assert_eq!(
            bootstrap_with(other, run).unwrap() as usize,
            first as usize + 8
        );
        assert_eq!(*ORDER.lock().unwrap(), [2, 1]);

        std::thread::spawn(move || {
            let answer = bootstrap_with(at, |func| {
                ORDER.lock().unwrap().push(func as u8);
                Ok(())
            })
            .unwrap();
            assert!(!answer.is_null());
        })
        .join()
        .unwrap();
        assert_eq!(*ORDER.lock().unwrap(), [2, 1, 2, 1]);
    }

    #[test]
    fn failed_constructor_is_not_retried_or_exposed() {
        let at = register_one(
            Template {
                initial_va: 0,
                initial_size: 0,
                total_size: 64,
                align: 16,
                init_funcs: vec![1, 2, 3],
            },
            0,
        );
        let image = SLOTS.read().unwrap()[&at].image;
        let other = register_slot(image, 8);
        let mut called = Vec::new();
        let result = bootstrap_with(at, |func| {
            called.push(func);
            if func == 2 {
                Err("failed".to_string())
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err(), "failed");
        assert_eq!(called, [3, 2]);
        assert!(bootstrap_with(at, |_| panic!("retried")).unwrap().is_null());
        assert!(
            bootstrap_with(other, |_| panic!("retried"))
                .unwrap()
                .is_null()
        );
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
                resolved.addr, bootstrap_thunk as *const () as usize as u64,
                "{name} must be bound to the thunk, not to the plain function"
            );
            assert_ne!(
                resolved.addr, bootstrap as *const () as usize as u64,
                "{name} is bound to a C function that clobbers the guest's registers"
            );
        }
    }
}
