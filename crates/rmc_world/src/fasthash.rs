//! A small, fixed hasher for the maps keyed by chunk and column position.
//!
//! The season sweeps look a chunk up for every cell they read, so the
//! default SipHash cost most of their time (F173). Keys here are a few
//! small integers chosen by the world, not by an attacker, so a
//! multiply-rotate hash (the Fx scheme) is enough. Nothing that affects
//! output iterates these maps unsorted, so the order they hash to does not
//! matter.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// The Fx multiplier (from Firefox and rustc).
const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
/// The Fx rotation between words.
const ROTATE: u32 = 5;

/// Multiply-rotate hasher over the integers written to it.
#[derive(Clone, Copy, Debug, Default)]
pub struct PosHasher {
    hash: u64,
}

impl PosHasher {
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(ROTATE) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for PosHasher {
    fn finish(&self) -> u64 {
        self.hash
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.add(u64::from(b));
        }
    }

    fn write_u8(&mut self, v: u8) {
        self.add(u64::from(v));
    }

    fn write_u16(&mut self, v: u16) {
        self.add(u64::from(v));
    }

    fn write_u32(&mut self, v: u32) {
        self.add(u64::from(v));
    }

    fn write_u64(&mut self, v: u64) {
        self.add(v);
    }

    fn write_usize(&mut self, v: usize) {
        self.add(v as u64);
    }

    fn write_i32(&mut self, v: i32) {
        self.add(u64::from(v as u32));
    }
}

/// A `HashMap` hashed with [`PosHasher`].
pub type PosMap<K, V> = HashMap<K, V, BuildHasherDefault<PosHasher>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::BuildHasher;

    use crate::coords::ChunkPos;

    fn hash(pos: ChunkPos) -> u64 {
        BuildHasherDefault::<PosHasher>::default().hash_one(pos)
    }

    #[test]
    fn neighboring_chunks_hash_apart() {
        let mut seen = std::collections::BTreeSet::new();
        for x in -4..4 {
            for y in -4..4 {
                for z in -4..4 {
                    assert!(seen.insert(hash(ChunkPos::new(x, y, z))), "({x}, {y}, {z})");
                }
            }
        }
    }

    #[test]
    fn hash_is_fixed() {
        // The same key hashes the same in every process (no random state).
        assert_eq!(hash(ChunkPos::new(1, 2, 3)), hash(ChunkPos::new(1, 2, 3)));
        let mut map: PosMap<ChunkPos, u32> = PosMap::default();
        map.insert(ChunkPos::new(-1, 0, 2), 7);
        assert_eq!(map.get(&ChunkPos::new(-1, 0, 2)), Some(&7));
    }
}
