//! A fast hasher for the maps a draw or encoder consults, keyed by small
//! structs of handles and enums. The standard library's SipHash resists
//! keys an attacker chooses, which these are not, and costs more than the
//! lookup it serves.

use std::hash::{BuildHasherDefault, Hasher};

#[derive(Default, Clone, Copy)]
pub(crate) struct FastHasher(u64);

/// A multiply-fold mix, as `objc::address::mix`, whose high bits depend on
/// every input bit.
#[inline]
pub(crate) fn mix(n: u64) -> u64 {
    let product = u128::from(n) * 0x9E37_79B9_7F4A_7C15;
    (product as u64) ^ ((product >> 64) as u64)
}

impl Hasher for FastHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
    }

    // Every field folds into what came before, so no field is lost.
    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.0 = mix(self.0.rotate_left(23) ^ n);
    }

    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.write_u64(u64::from(n));
    }

    #[inline]
    fn write_u16(&mut self, n: u16) {
        self.write_u64(u64::from(n));
    }

    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.write_u64(u64::from(n));
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    #[inline]
    fn write_i32(&mut self, n: i32) {
        self.write_u64(n as u32 as u64);
    }

    #[inline]
    fn write_i64(&mut self, n: i64) {
        self.write_u64(n as u64);
    }
}

pub(crate) type FastMap<K, V> = std::collections::HashMap<K, V, BuildHasherDefault<FastHasher>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{Hash, Hasher};

    fn hash(value: impl Hash) -> u64 {
        let mut hasher = FastHasher::default();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn every_field_of_a_key_changes_its_hash() {
        let base = hash((1u32, 2u64, 3u8));
        assert_ne!(base, hash((9u32, 2u64, 3u8)));
        assert_ne!(base, hash((1u32, 9u64, 3u8)));
        assert_ne!(base, hash((1u32, 2u64, 9u8)));
    }

    #[test]
    fn fields_in_another_order_hash_differently() {
        assert_ne!(hash((1u64, 2u64)), hash((2u64, 1u64)));
    }
}
