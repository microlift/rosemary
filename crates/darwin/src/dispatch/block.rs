use std::sync::atomic::{AtomicU64, AtomicPtr, Ordering};

/// Called immediately before a block's guest code is entered, on whichever
/// thread is about to run it.
///
/// **This exists because a dispatch pool worker is a thread this runtime
/// started, and it was invisible as a guest thread.** `DarwinThread::enter`
/// gives it a Darwin thread pointer - so `pthread_self`, `errno` and a stack
/// canary read correctly - but registers nothing, so `rosetta3::thread::
/// current()` is `None` on it. Guest code entered from here therefore ran with
/// no thread state, which costs at least the indirect-call cache
/// (`rosetta3::vm::dispatch_indirect` takes `None` and skips its lookup) and
/// leaves anything keyed on the current thread unanswered.
///
/// `rosetta3` installs [`adopt_thread`], because `darwin` must not depend on it.
/// The default is null, which is exactly the previous behaviour, so this is inert
/// until something supplies it.
static ADOPT_THREAD: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// The hook [`ADOPT_THREAD`] calls, as a `fn()`. Registered by `rosetta3`.
type AdoptThread = fn();

/// Install the "about to enter guest code" hook. `None` restores the default.
pub fn set_thread_adopt_hook(hook: Option<AdoptThread>) {
    ADOPT_THREAD.store(
        hook.map_or(std::ptr::null_mut(), |f| f as *mut ()),
        Ordering::Release,
    );
}

/// Run the hook, if one is installed. Best-effort: it must not be able to stop
/// the block from running, so its failure is the caller's to have noticed rather
/// than something to propagate from here.
#[inline]
pub(crate) fn adopt_thread() {
    let raw = ADOPT_THREAD.load(Ordering::Acquire);
    if raw.is_null() {
        return;
    }
    // SAFETY: a `fn()` installed by `set_thread_adopt_hook`, which only ever
    // stores the address of a `fn()` item.
    let f: AdoptThread = unsafe { std::mem::transmute(raw) };
    f();
}

/// Byte offset of `invoke` in a `Block_layout`, after `isa`, `flags` and `reserved`.
const INVOKE_OFFSET: u64 = 16;

/// `_Block_copy` and `_Block_release`, as the crate that owns them registers them.
static COPY: AtomicU64 = AtomicU64::new(0);
static RELEASE: AtomicU64 = AtomicU64::new(0);

/// Tell this module which block runtime the process is using.
///
/// Called once at start-up by the crate that owns the runtime. Until it is, the
/// block-taking entry points refuse rather than guess - see [`copy`].
pub fn set_runtime(copy: u64, release: u64) {
    COPY.store(copy, Ordering::Release);
    RELEASE.store(release, Ordering::Release);
}

/// A heap copy of `block` that outlives the caller, or `None` if no runtime is
/// registered.
///
/// The `None` is not a theoretical branch: a caller that treated it as success would
/// queue a pointer into a dead stack frame and crash later, somewhere else.
pub fn copy(block: u64) -> Option<u64> {
    let copy = COPY.load(Ordering::Acquire);
    if copy == 0 || block == 0 {
        return None;
    }
    // SAFETY: `copy` is the registered `_Block_copy`, whose prototype is one pointer
    // in and one out.
    let copied = unsafe { std::mem::transmute::<u64, extern "C" fn(u64) -> u64>(copy)(block) };
    (copied != 0).then_some(copied)
}

/// Drop a reference taken by [`copy`].
pub fn release(block: u64) {
    let release = RELEASE.load(Ordering::Acquire);
    if release == 0 || block == 0 {
        return;
    }
    // SAFETY: `release` is the registered `_Block_release`, and `block` came from
    // the matching `_Block_copy`.
    unsafe { std::mem::transmute::<u64, extern "C" fn(u64)>(release)(block) };
}

/// Call a block: its `invoke`, with the block itself as the first argument.
///
/// # Safety
///
/// `block` is a live block literal whose `invoke` is callable from this thread. For a
/// guest's block that means its code pointers have already been replaced by thunks,
/// which is the caller's job and happens before it reaches this crate.
pub unsafe fn invoke(block: u64) {
    if block == 0 {
        return;
    }
    // Before the guest's code is entered, and on this thread, which may be one
    // this runtime started rather than one the guest created. See
    // [`adopt_thread`].
    adopt_thread();
    // SAFETY: the caller's guarantee; `invoke` is at a fixed offset by the Block ABI.
    let entry = unsafe { ((block + INVOKE_OFFSET) as *const u64).read() };
    if entry == 0 {
        return;
    }
    // SAFETY: a `void (*)(void *)` by the Block ABI for a block taking no arguments,
    // which is what every block libdispatch runs is.
    unsafe { std::mem::transmute::<u64, extern "C" fn(u64)>(entry)(block) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;
    use std::sync::atomic::AtomicU32;

    static RAN: AtomicU32 = AtomicU32::new(0);

    #[repr(C)]
    struct Literal {
        isa: u64,
        flags: u32,
        reserved: u32,
        invoke: u64,
        descriptor: u64,
    }

    extern "C" fn body(_block: u64) {
        RAN.fetch_add(1, Ordering::AcqRel);
    }

    /// `invoke` must be found at the ABI's offset and called with the block itself.
    #[test]
    fn a_block_is_invoked_through_its_literal() {
        RAN.store(0, Ordering::Release);
        let literal = Literal {
            isa: 0,
            flags: 0,
            reserved: 0,
            invoke: body as *const () as usize as u64,
            descriptor: 0,
        };
        assert_eq!(
            &literal.invoke as *const u64 as u64 - &literal as *const _ as u64,
            INVOKE_OFFSET,
            "the Block ABI puts invoke at offset 16"
        );
        // SAFETY: a live literal whose `invoke` is a host function.
        unsafe { invoke(&literal as *const _ as u64) };
        assert_eq!(RAN.load(Ordering::Acquire), 1);
    }

    /// With no runtime registered a copy must fail rather than alias the original,
    /// which would be a pointer into the caller's dead frame.
    #[test]
    fn a_copy_without_a_runtime_is_refused() {
        let saved = COPY.swap(0, Ordering::AcqRel);
        assert_eq!(copy(0x1000), None);
        COPY.store(saved, Ordering::Release);
    }

    #[test]
    fn invoking_nothing_is_not_a_crash() {
        // SAFETY: null is handled without a dereference.
        unsafe { invoke(0) };
        let _ = std::ptr::null::<c_void>();
    }
}
