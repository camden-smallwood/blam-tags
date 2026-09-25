//! A fast hash for maps keyed by small integers the crate computes itself —
//! grid cells, vertex-index edges — where the default SipHash, built to
//! resist adversarial keys, is most of the cost of a lookup. Lookups only
//! differ; nothing that iterates a map in hash order should use it.

use std::hash::{BuildHasherDefault, Hasher};

/// `BuildHasher` for [`IntHasher`].
pub(crate) type IntHash = BuildHasherDefault<IntHasher>;

/// A `HashMap` keyed by integers or tuples of them.
pub(crate) type IntMap<K, V> = std::collections::HashMap<K, V, IntHash>;

/// Multiply-rotate mixing, one step per integer written.
#[derive(Default)]
pub(crate) struct IntHasher(u64);

impl Hasher for IntHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u64(byte as u64);
        }
    }

    fn write_u32(&mut self, value: u32) {
        self.write_u64(value as u64);
    }

    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }

    fn write_i64(&mut self, value: i64) {
        self.write_u64(value as u64);
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}
