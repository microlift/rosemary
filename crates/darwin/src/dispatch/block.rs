use std::sync::atomic::{AtomicU64, Ordering};

/// Byte offset of `invoke` in a `Block_layout`, after `isa`, `flags` and `reserved`.
pub(crate) const INVOKE_OFFSET: u64 = 16;

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
