use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};

use super::object::{Header, Kind};

/// `#[repr(C)]` is load-bearing, not decoration: [`super::object`] recovers this
/// object by casting a `*mut Header`, which is only the same address as the whole
/// object if `header` is guaranteed to come first. A plain Rust struct may reorder
/// its fields, and one that did would read another field's bytes as the refcount.
#[repr(C)]
pub struct Data {
    /// Read through a `*mut Header` rather than through this field, which is what
    /// makes it look unused: it must be first, and its bytes are the object
    /// protocol's.
    #[allow(dead_code)]
    header: Header,
    /// The copy, when this object took one.
    owned: Vec<u8>,
    /// The caller's buffer and the destructor that frees it, when it did not.
    borrowed: *const c_void,
    /// Recorded rather than derived: a borrowed buffer has no length of its own
    /// here, and `dispatch_data_get_size` must answer for both kinds alike.
    size: usize,
    destructor: u64,
    queue: u64,
}

/// `DISPATCH_DATA_DESTRUCTOR_FREE`, which asks for plain `free` rather than a block.
///
/// Darwin spells it as the address of a known block, so a guest passes a pointer this
/// process never defined. It is recognised by the symbol this module publishes.
pub fn destructor_free() -> u64 {
    static FREE: AtomicU64 = AtomicU64::new(0);
    let existing = FREE.load(Ordering::Acquire);
    if existing != 0 {
        return existing;
    }
    let fresh = Box::into_raw(Box::new(0u64)) as u64;
    match FREE.compare_exchange(0, fresh, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => fresh,
        Err(won) => {
            // SAFETY: this thread's allocation, which no one else ever saw.
            drop(unsafe { Box::from_raw(fresh as *mut u64) });
            won
        }
    }
}

/// `dispatch_data_t dispatch_data_create(const void *buffer, size_t size,
/// dispatch_queue_t queue, dispatch_block_t destructor)`.
///
/// # Safety
///
/// `buffer` addresses `size` readable bytes.
pub unsafe extern "C" fn create(
    buffer: *const c_void,
    size: usize,
    queue: u64,
    destructor: u64,
) -> u64 {
    let destructor = if destructor != 0 && destructor != destructor_free() {
        let Some(copied) = super::block::copy(destructor) else {
            return 0;
        };
        copied
    } else {
        destructor
    };
    let queue = hold_queue(queue);
    let size = if buffer.is_null() { 0 } else { size };
    let copies = destructor == 0;
    let owned = match copies && !buffer.is_null() && size != 0 {
        // SAFETY: the caller states the buffer has `size` readable bytes.
        true => unsafe { std::slice::from_raw_parts(buffer as *const u8, size) }.to_vec(),
        false => Vec::new(),
    };
    let mut data = Data {
        header: Header::new(Kind::Data),
        owned,
        borrowed: match copies {
            true => std::ptr::null(),
            false => buffer,
        },
        size,
        destructor,
        queue,
    };
    if size == 0 {
        data.destroy_buffer();
    }
    Box::into_raw(Box::new(data)) as u64
}

impl Data {
    fn destroy_buffer(&mut self) {
        let destructor = std::mem::take(&mut self.destructor);
        if destructor == destructor_free() {
            // The FREE sentinel transfers ownership to the guest allocator.
            unsafe { crate::heap::free(self.borrowed as *mut c_void) };
        } else if destructor != 0 {
            run_destructor(self.queue, destructor);
        }
        self.borrowed = std::ptr::null();
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        self.destroy_buffer();
        // SAFETY: the reference `hold_queue` took.
        unsafe { super::object::release_entry(self.queue) };
    }
}

fn hold_queue(queue: u64) -> u64 {
    let queue = if queue == 0 {
        unsafe { super::queue::get_global_queue(0, 0) }
    } else {
        queue
    };
    super::object::retain(queue);
    queue
}

fn run_destructor(queue: u64, destructor: u64) {
    super::queue::async_owned(queue, destructor);
}

/// The bytes this object stands for, however it came to own them.
pub fn bytes(handle: u64) -> Option<(*const u8, usize)> {
    // SAFETY: a live object, by the caller of the entry point above this.
    let header = unsafe { super::object::typed(handle, Kind::Data) }?;
    // SAFETY: a `Header` of kind `Data` is the first field of a `Data`.
    let data = unsafe { &*(header as *const Header as *const Data) };
    match data.borrowed.is_null() {
        true => Some((data.owned.as_ptr(), data.owned.len())),
        false => Some((data.borrowed as *const u8, data.size)),
    }
}

/// `size_t dispatch_data_get_size(dispatch_data_t)`.
pub unsafe extern "C" fn get_size(handle: u64) -> usize {
    bytes(handle).map(|(_, len)| len).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::super::object;
    use super::*;

    /// A null destructor means the buffer belongs to the caller, so the object must
    /// still read correctly once that buffer is gone.
    #[test]
    fn a_default_destructor_copies_the_bytes() {
        let handle = {
            let source = vec![1u8, 2, 3, 4];
            // SAFETY: a live buffer of the stated length.
            unsafe { create(source.as_ptr() as *const c_void, source.len(), 0, 0) }
        };
        // SAFETY: a live object.
        assert_eq!(unsafe { get_size(handle) }, 4);
        let (at, len) = bytes(handle).expect("a data object");
        // SAFETY: the copy this object owns.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(at, len) },
            &[1, 2, 3, 4]
        );
        // SAFETY: giving up the create reference.
        unsafe { object::release(handle) };
    }

    /// `DISPATCH_DATA_DESTRUCTOR_FREE` has one address for the process, or a guest's
    /// comparison against it never matches.
    #[test]
    fn the_free_destructor_is_one_address() {
        assert_eq!(destructor_free(), destructor_free());
        assert_ne!(destructor_free(), 0);
    }

    /// A handed-over buffer is freed by the destructor named, not copied.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_free_destructor_takes_the_buffer() {
        // SAFETY: the guest's `malloc` is what `DISPATCH_DATA_DESTRUCTOR_FREE`
        // promises; the host's is another allocator in a test.
        let buffer = unsafe { crate::malloc::malloc(8) };
        // SAFETY: a live malloc'd buffer handed over with the free destructor.
        let handle = unsafe { create(buffer, 8, 0, destructor_free()) };
        // SAFETY: the release frees the buffer through the destructor.
        unsafe { object::release(handle) };
    }
}
