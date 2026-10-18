//! Chunk storage (SAD §7): a 32³ block of cells, either one material or a
//! bit-packed palette.

use crate::coords::{CHUNK_VOLUME, LocalPos};
use crate::materials::MaterialId;

/// Bit widths a paletted chunk may use. Each divides 64, so no entry spans two words.
pub const PALETTE_BITS: [u8; 5] = [1, 2, 4, 8, 16];

/// One chunk of the world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub storage: ChunkStorage,
    /// Increments on every applied change.
    pub revision: u64,
    /// Differs from worldgen output, so it is saved.
    pub modified: bool,
}

impl Chunk {
    /// A chunk with revision 0 that is not modified.
    pub fn new(storage: ChunkStorage) -> Self {
        Self {
            storage,
            revision: 0,
            modified: false,
        }
    }

    /// A chunk filled with one material.
    pub fn uniform(id: MaterialId) -> Self {
        Self::new(ChunkStorage::Uniform(id))
    }
}

/// Cell contents of a chunk.
///
/// `PartialEq` is structural: a `Uniform` chunk and an equivalent `Paletted`
/// chunk compare unequal. Compare contents cell by cell, or by hash (F012).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChunkStorage {
    /// Every cell holds this material.
    Uniform(MaterialId),
    /// `data` packs `CHUNK_VOLUME` palette indices of `bits` each, little end
    /// of each word first, in local linear index order.
    Paletted {
        palette: Vec<MaterialId>,
        bits: u8,
        data: Vec<u64>,
    },
}

impl ChunkStorage {
    /// Material at `local`.
    pub fn get(&self, local: LocalPos) -> MaterialId {
        match self {
            ChunkStorage::Uniform(id) => *id,
            ChunkStorage::Paletted {
                palette,
                bits,
                data,
            } => palette[read_packed(data, *bits, local.index() as usize)],
        }
    }

    /// Writes `id` at `local` and returns the previous material. Grows the
    /// palette and promotes the bit width as needed; a `Uniform` chunk
    /// promotes on its first differing write.
    pub fn set(&mut self, local: LocalPos, id: MaterialId) -> MaterialId {
        let i = local.index() as usize;
        match self {
            ChunkStorage::Uniform(fill) => {
                let prev = *fill;
                if prev != id {
                    let bits = PALETTE_BITS[0];
                    let mut data = vec![0; packed_len(bits)];
                    write_packed(&mut data, bits, i, 1);
                    *self = ChunkStorage::Paletted {
                        palette: vec![prev, id],
                        bits,
                        data,
                    };
                }
                prev
            }
            ChunkStorage::Paletted {
                palette,
                bits,
                data,
            } => {
                let prev = palette[read_packed(data, *bits, i)];
                if prev == id {
                    return prev;
                }
                let slot = match palette.iter().position(|&m| m == id) {
                    Some(slot) => slot,
                    None => {
                        palette.push(id);
                        let need = bits_for(palette.len());
                        if need > *bits {
                            *data = repack(data, *bits, need);
                            *bits = need;
                        }
                        palette.len() - 1
                    }
                };
                write_packed(data, *bits, i, slot);
                prev
            }
        }
    }

    /// Rebuilds the palette from the cells: drops unused entries, orders the
    /// rest by first appearance in local linear index order, and demotes to
    /// the smallest bit width. A chunk left with one material becomes
    /// `Uniform`. Contents are unchanged.
    pub fn compact(&mut self) {
        let ChunkStorage::Paletted {
            palette,
            bits,
            data,
        } = self
        else {
            return;
        };
        // Old palette slot -> new palette slot, assigned on first appearance.
        let mut remap: Vec<Option<usize>> = vec![None; palette.len()];
        let mut new_palette = Vec::new();
        let mut slots = Vec::with_capacity(CHUNK_VOLUME);
        for i in 0..CHUNK_VOLUME {
            let old = read_packed(data, *bits, i);
            let slot = *remap[old].get_or_insert_with(|| {
                new_palette.push(palette[old]);
                new_palette.len() - 1
            });
            slots.push(slot);
        }
        if new_palette.len() == 1 {
            *self = ChunkStorage::Uniform(new_palette[0]);
            return;
        }
        let new_bits = bits_for(new_palette.len());
        let mut new_data = vec![0; packed_len(new_bits)];
        for (i, slot) in slots.into_iter().enumerate() {
            write_packed(&mut new_data, new_bits, i, slot);
        }
        *palette = new_palette;
        *bits = new_bits;
        *data = new_data;
    }
}

/// Smallest allowed bit width that can index `len` palette entries.
fn bits_for(len: usize) -> u8 {
    PALETTE_BITS
        .into_iter()
        .find(|&b| len <= 1usize << b)
        .expect("a palette of distinct u16 ids never exceeds 65536 entries")
}

/// Number of `u64` words holding `CHUNK_VOLUME` entries of `bits` each.
fn packed_len(bits: u8) -> usize {
    CHUNK_VOLUME * bits as usize / 64
}

fn read_packed(data: &[u64], bits: u8, i: usize) -> usize {
    let per_word = 64 / bits as usize;
    let shift = (i % per_word) * bits as usize;
    let mask = (1u64 << bits) - 1;
    ((data[i / per_word] >> shift) & mask) as usize
}

fn write_packed(data: &mut [u64], bits: u8, i: usize, value: usize) {
    let per_word = 64 / bits as usize;
    let shift = (i % per_word) * bits as usize;
    let mask = (1u64 << bits) - 1;
    let word = &mut data[i / per_word];
    *word = (*word & !(mask << shift)) | ((value as u64 & mask) << shift);
}

/// Re-encodes every entry of `data` from `from` bits to `to` bits.
fn repack(data: &[u64], from: u8, to: u8) -> Vec<u64> {
    let mut out = vec![0; packed_len(to)];
    for i in 0..CHUNK_VOLUME {
        write_packed(&mut out, to, i, read_packed(data, from, i));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn all_locals() -> impl Iterator<Item = LocalPos> {
        (0..CHUNK_VOLUME as u32).map(|i| LocalPos::from_index(i as u16))
    }

    /// Checks the representation invariants of a paletted chunk.
    fn assert_well_formed(s: &ChunkStorage) {
        if let ChunkStorage::Paletted {
            palette,
            bits,
            data,
        } = s
        {
            assert!(PALETTE_BITS.contains(bits), "bits {bits} not allowed");
            assert!(
                palette.len() <= 1usize << *bits,
                "palette of {} does not fit {bits} bits",
                palette.len()
            );
            assert!(
                *bits == 1 || palette.len() > 1usize << (*bits / 2),
                "bits {bits} wider than needed for {} entries",
                palette.len()
            );
            assert_eq!(data.len(), CHUNK_VOLUME * *bits as usize / 64);
            let mut sorted = palette.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), palette.len(), "palette has duplicates");
        }
    }

    #[test]
    fn uniform_promotes_on_first_write() {
        let stone = MaterialId(4);
        let dirt = MaterialId(2);
        let mut s = ChunkStorage::Uniform(stone);
        let p = LocalPos::new(3, 17, 31);

        // Writing the same material keeps the chunk uniform.
        assert_eq!(s.set(p, stone), stone);
        assert_eq!(s, ChunkStorage::Uniform(stone));

        // The first differing write promotes to a 1-bit palette.
        assert_eq!(s.set(p, dirt), stone);
        match &s {
            ChunkStorage::Paletted { palette, bits, .. } => {
                assert_eq!(*bits, 1);
                assert_eq!(palette.as_slice(), &[stone, dirt]);
            }
            ChunkStorage::Uniform(_) => panic!("did not promote"),
        }
        assert_well_formed(&s);
        for l in all_locals() {
            let want = if l == p { dirt } else { stone };
            assert_eq!(s.get(l), want, "at {l:?}");
        }
        assert_eq!(s.set(p, stone), dirt);
        assert_eq!(s.get(p), stone);
    }

    proptest! {
        #[test]
        fn set_get_roundtrip_proptest(
            fill in 0u16..8,
            ops in prop::collection::vec(
                (0u16..CHUNK_VOLUME as u16, prop_oneof![0u16..8, any::<u16>()]),
                0..400,
            ),
        ) {
            let mut s = ChunkStorage::Uniform(MaterialId(fill));
            let mut model = vec![MaterialId(fill); CHUNK_VOLUME];
            for (i, id) in ops {
                let id = MaterialId(id);
                let prev = s.set(LocalPos::from_index(i), id);
                prop_assert_eq!(prev, model[i as usize]);
                model[i as usize] = id;
                prop_assert_eq!(s.get(LocalPos::from_index(i)), id);
            }
            assert_well_formed(&s);
            for (i, want) in model.iter().enumerate() {
                prop_assert_eq!(s.get(LocalPos::from_index(i as u16)), *want, "at index {}", i);
            }
        }
    }

    #[test]
    fn bits_promote_through_16() {
        let mut s = ChunkStorage::Uniform(MaterialId(0));
        let mut model = vec![MaterialId(0); CHUNK_VOLUME];
        let mut seen = Vec::new();
        // Write N distinct materials to spread-out cells; the palette holds N+1.
        for n in 1u16..=300 {
            let i = (u32::from(n) * 109 % CHUNK_VOLUME as u32) as u16;
            s.set(LocalPos::from_index(i), MaterialId(n));
            model[i as usize] = MaterialId(n);
            let ChunkStorage::Paletted { palette, bits, .. } = &s else {
                panic!("still uniform after {n} writes");
            };
            assert_eq!(palette.len(), usize::from(n) + 1);
            if seen.last() != Some(bits) {
                seen.push(*bits);
                // Content survives every repack.
                for (j, want) in model.iter().enumerate() {
                    assert_eq!(s.get(LocalPos::from_index(j as u16)), *want, "index {j}");
                }
            }
            assert_well_formed(&s);
        }
        assert_eq!(seen, vec![1, 2, 4, 8, 16]);

        // Widest ids round-trip at 16 bits.
        let top = LocalPos::from_index((CHUNK_VOLUME - 1) as u16);
        s.set(top, MaterialId(u16::MAX));
        assert_eq!(s.get(top), MaterialId(u16::MAX));
        assert_eq!(s.get(LocalPos::from_index(109)), MaterialId(1));
    }

    proptest! {
        #[test]
        fn compact_preserves_contents_proptest(
            fill in 0u16..8,
            ops in prop::collection::vec(
                (0u16..CHUNK_VOLUME as u16, prop_oneof![0u16..8, any::<u16>()]),
                0..400,
            ),
            // Overwrite a prefix of the ops with `fill` so some palette entries go unused.
            undo in 0usize..400,
        ) {
            let mut s = ChunkStorage::Uniform(MaterialId(fill));
            let mut model = vec![MaterialId(fill); CHUNK_VOLUME];
            for &(i, id) in &ops {
                s.set(LocalPos::from_index(i), MaterialId(id));
                model[i as usize] = MaterialId(id);
            }
            for &(i, _) in ops.iter().take(undo) {
                s.set(LocalPos::from_index(i), MaterialId(fill));
                model[i as usize] = MaterialId(fill);
            }

            let mut c = s.clone();
            c.compact();
            assert_well_formed(&c);
            for (i, want) in model.iter().enumerate() {
                prop_assert_eq!(c.get(LocalPos::from_index(i as u16)), *want, "at index {}", i);
            }

            let mut used = model.clone();
            used.sort_unstable();
            used.dedup();
            match &c {
                ChunkStorage::Uniform(id) => prop_assert_eq!(used.as_slice(), &[*id]),
                ChunkStorage::Paletted { palette, .. } => {
                    // Exactly the used materials, in first-appearance order.
                    let mut first = Vec::new();
                    for m in &model {
                        if !first.contains(m) {
                            first.push(*m);
                        }
                    }
                    prop_assert!(first.len() > 1);
                    prop_assert_eq!(palette, &first);
                }
            }

            // Compacting is idempotent, and set keeps working afterwards.
            let mut again = c.clone();
            again.compact();
            prop_assert_eq!(&again, &c);
            let p = LocalPos::from_index(12345);
            prop_assert_eq!(c.set(p, MaterialId(9999)), model[12345]);
            prop_assert_eq!(c.get(p), MaterialId(9999));
            assert_well_formed(&c);
        }
    }

    #[test]
    fn compact_demotes_bits() {
        let base = MaterialId(0);
        let mut s = ChunkStorage::Uniform(base);
        // 300 distinct materials push the chunk to 16 bits.
        let cells: Vec<u16> = (1u16..=300)
            .map(|n| (u32::from(n) * 109 % CHUNK_VOLUME as u32) as u16)
            .collect();
        for (n, &i) in (1u16..).zip(&cells) {
            s.set(LocalPos::from_index(i), MaterialId(n));
        }
        assert!(matches!(s, ChunkStorage::Paletted { bits: 16, .. }));

        // Overwrite all but the first k with `base`; compact demotes to the
        // width that holds k + 1 entries.
        for (k, want_bits) in [(255usize, 8u8), (15, 4), (3, 2), (1, 1)] {
            for &i in &cells[k..] {
                s.set(LocalPos::from_index(i), base);
            }
            // set alone never demotes.
            assert!(matches!(s, ChunkStorage::Paletted { bits: 16, .. }));
            let mut c = s.clone();
            c.compact();
            assert_well_formed(&c);
            let ChunkStorage::Paletted {
                palette,
                bits,
                data,
            } = &c
            else {
                panic!("{} materials compacted to Uniform", k + 1);
            };
            assert_eq!(*bits, want_bits, "k = {k}");
            assert_eq!(palette.len(), k + 1);
            assert_eq!(data.len(), CHUNK_VOLUME * want_bits as usize / 64);
            for (n, &i) in (1u16..).zip(&cells) {
                let want = if usize::from(n) <= k {
                    MaterialId(n)
                } else {
                    base
                };
                assert_eq!(c.get(LocalPos::from_index(i)), want, "k = {k}, cell {i}");
            }
        }

        // One material left: compact returns to Uniform.
        s.set(LocalPos::from_index(cells[0]), base);
        s.compact();
        assert_eq!(s, ChunkStorage::Uniform(base));

        // A paletted chunk whose fill was entirely overwritten keeps only the new material.
        let mut s = ChunkStorage::Uniform(MaterialId(1));
        for i in 0..CHUNK_VOLUME as u16 {
            s.set(LocalPos::from_index(i), MaterialId(2));
        }
        s.compact();
        assert_eq!(s, ChunkStorage::Uniform(MaterialId(2)));

        // Uniform is already compact.
        let mut u = ChunkStorage::Uniform(MaterialId(7));
        u.compact();
        assert_eq!(u, ChunkStorage::Uniform(MaterialId(7)));
    }
}
