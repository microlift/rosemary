use std::sync::Arc;

#[repr(C, align(8))]
pub struct Guest<const N: usize>([u8; N]);

// SAFETY: the bytes are only ever touched through the `pthread_*` entry points, whose
// whole job is to make concurrent access to them safe.
unsafe impl<const N: usize> Send for Guest<N> {}
unsafe impl<const N: usize> Sync for Guest<N> {}

impl<const N: usize> Guest<N> {
    pub fn zeroed() -> Arc<Self> {
        Arc::new(Self([0; N]))
    }

    pub fn statically_initialised(signature: u64) -> Arc<Self> {
        let mut object = Self([0; N]);
        object.0[..8].copy_from_slice(&signature.to_le_bytes());
        Arc::new(object)
    }

    /// A byte-for-byte copy of another object, which POSIX does not allow and
    /// Darwin tolerates.
    pub fn copy_of(other: &Self) -> Arc<Self> {
        Arc::new(Self(other.0))
    }

    pub fn at(&self) -> u64 {
        self as *const Self as usize as u64
    }
}
