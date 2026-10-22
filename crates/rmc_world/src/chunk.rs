//! Chunk storage (SAD §7): a 32³ block of cells, either one material or a
//! bit-packed palette.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::coords::{CHUNK_VOLUME, LocalPos};
use crate::layers::{CellSet, ChunkLayers, N_SWEEPS, PlantState, plants_hash};
use crate::materials::MaterialId;

/// Bit widths a paletted chunk may use. Each divides 64, so no entry spans two words.
pub const PALETTE_BITS: [u8; 5] = [1, 2, 4, 8, 16];

/// One chunk of the world: cells plus the ecology state of SAD-P2 §6.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub storage: ChunkStorage,
    /// Per-cell layers; absent layers read 0.
    pub layers: ChunkLayers,
    /// Plant state by local index.
    pub plants: BTreeMap<u16, PlantState>,
    /// Season-scoped freeze-thaw bits. Not hashed or saved.
    pub freeze_thaw: CellSet,
    /// Cells each sweep visits. Not hashed or saved.
    pub active: [CellSet; N_SWEEPS],
    /// Water-sweep cells whose last visit moved nothing, with no change to
    /// them or their 6 face neighbors since: the water sweep skips them
    /// (F173). Kept by the world ([`World`](crate::world::World) wakes a
    /// cell and its neighbors when a material or moisture changes). Not
    /// hashed or saved.
    pub water_settled: CellSet,
    /// Increments on every material change.
    pub revision: u64,
    /// Increments on every layer or plant-state change.
    pub eco_revision: u64,
    /// Differs from worldgen output, so it is saved.
    pub modified: bool,
}

impl Chunk {
    /// A chunk with revisions 0, no layers or plants, that is not modified.
    pub fn new(storage: ChunkStorage) -> Self {
        Self {
            storage,
            layers: ChunkLayers::default(),
            plants: BTreeMap::new(),
            freeze_thaw: CellSet::default(),
            active: Default::default(),
            water_settled: CellSet::default(),
            revision: 0,
            eco_revision: 0,
            modified: false,
        }
    }

    /// A chunk filled with one material.
    pub fn uniform(id: MaterialId) -> Self {
        Self::new(ChunkStorage::Uniform(id))
    }

    /// Content hash of the cells, layers and plants (SAD-P2 §6). Revisions,
    /// `modified`, freeze-thaw bits and active sets are not part of it.
    ///
    /// A chunk whose layers all read 0 and that has no plants hashes as its
    /// storage alone ([`ChunkStorage::hash`]), so Phase 1 hashes are
    /// unchanged. Otherwise the hash is xxh3 over a prefix and the storage,
    /// layer and plant hashes.
    pub fn hash(&self) -> u64 {
        if self.layers.is_zero() && self.plants.is_empty() {
            return self.storage.hash();
        }
        const PREFIX: &[u8] = b"rmc.chunk.eco.v1";
        let mut buf = Vec::with_capacity(PREFIX.len() + 3 * 8);
        buf.extend_from_slice(PREFIX);
        for h in [
            self.storage.hash(),
            self.layers.hash(),
            plants_hash(&self.plants),
        ] {
            buf.extend_from_slice(&h.to_le_bytes());
        }
        xxhash_rust::xxh3::xxh3_64(&buf)
    }

    /// Bytes the chunk owns on the heap: storage, layers, bitsets, and the
    /// plant entries (map node overhead excluded).
    pub fn heap_bytes(&self) -> usize {
        self.storage.heap_bytes()
            + self.layers.heap_bytes()
            + self.freeze_thaw.heap_bytes()
            + self.active.iter().map(CellSet::heap_bytes).sum::<usize>()
            + self.water_settled.heap_bytes()
            + self.plants.len() * std::mem::size_of::<(u16, PlantState)>()
    }
}

/// Cell contents of a chunk.
///
/// `PartialEq` is structural: a `Uniform` chunk and an equivalent `Paletted`
/// chunk compare unequal. Compare contents cell by cell, or by [`ChunkStorage::hash`].
///
/// The serde form is structural too; storage decoded from outside the
/// process must pass [`ChunkStorage::is_well_formed`] before use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

    /// Number of cells holding `id`. Decodes the packed words in order
    /// instead of one `get` per cell; 0 at once when `id` is not in the
    /// palette.
    pub fn count(&self, id: MaterialId) -> usize {
        match self {
            ChunkStorage::Uniform(m) if *m == id => CHUNK_VOLUME,
            ChunkStorage::Uniform(_) => 0,
            ChunkStorage::Paletted {
                palette,
                bits,
                data,
            } => {
                let hit: Vec<bool> = palette.iter().map(|&m| m == id).collect();
                if !hit.contains(&true) {
                    return 0;
                }
                let bits = usize::from(*bits);
                let mask = (1u64 << bits) - 1;
                data.iter()
                    .map(|&word| {
                        (0..64 / bits)
                            .filter(|k| {
                                hit.get(((word >> (k * bits)) & mask) as usize) == Some(&true)
                            })
                            .count()
                    })
                    .sum()
            }
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
        if let ChunkStorage::Paletted {
            palette,
            bits,
            data,
        } = self
        {
            *self = compacted(palette, *bits, data);
        }
    }

    /// Whether the other methods can use this storage without panicking: a
    /// `Paletted` chunk needs an allowed bit width, exactly the data words
    /// for `CHUNK_VOLUME` cells, a non-empty palette that fits the width,
    /// and every cell's slot inside the palette. Storage built by
    /// [`set`](Self::set) always is; storage from the wire may not be.
    pub fn is_well_formed(&self) -> bool {
        match self {
            ChunkStorage::Uniform(_) => true,
            ChunkStorage::Paletted {
                palette,
                bits,
                data,
            } => {
                PALETTE_BITS.contains(bits)
                    && data.len() == packed_len(*bits)
                    && !palette.is_empty()
                    && palette.len() <= 1 << *bits
                    && (0..CHUNK_VOLUME).all(|i| read_packed(data, *bits, i) < palette.len())
            }
        }
    }

    /// Bytes this storage owns on the heap: the allocated capacity of the
    /// palette and data vectors. `Uniform` owns none.
    pub fn heap_bytes(&self) -> usize {
        match self {
            ChunkStorage::Uniform(_) => 0,
            ChunkStorage::Paletted { palette, data, .. } => {
                palette.capacity() * std::mem::size_of::<MaterialId>()
                    + data.capacity() * std::mem::size_of::<u64>()
            }
        }
    }

    /// xxh3 over the canonical paletted encoding of the cells, so a `Uniform`
    /// chunk and any `Paletted` chunk with the same contents hash the same.
    ///
    /// The canonical form is the [`compact`](Self::compact)ed one: palette in
    /// first-appearance order, smallest bit width. A single-material chunk is
    /// a one-entry palette with 0 bits and no data words.
    pub fn hash(&self) -> u64 {
        match self {
            ChunkStorage::Uniform(id) => canonical_hash(&[*id], 0, &[]),
            ChunkStorage::Paletted {
                palette,
                bits,
                data,
            } => match compacted(palette, *bits, data) {
                ChunkStorage::Uniform(id) => canonical_hash(&[id], 0, &[]),
                ChunkStorage::Paletted {
                    palette,
                    bits,
                    data,
                } => canonical_hash(&palette, bits, &data),
            },
        }
    }
}

/// The compacted form of a paletted chunk: unused entries dropped, the rest in
/// first-appearance order in local linear index order, smallest bit width,
/// `Uniform` if only one material is used.
fn compacted(palette: &[MaterialId], bits: u8, data: &[u64]) -> ChunkStorage {
    // Old palette slot -> new palette slot, assigned on first appearance.
    let mut remap: Vec<Option<usize>> = vec![None; palette.len()];
    let mut new_palette = Vec::new();
    for i in 0..CHUNK_VOLUME {
        let old = read_packed(data, bits, i);
        remap[old].get_or_insert_with(|| {
            new_palette.push(palette[old]);
            new_palette.len() - 1
        });
    }
    if new_palette.len() == 1 {
        return ChunkStorage::Uniform(new_palette[0]);
    }
    let new_bits = bits_for(new_palette.len());
    let mut new_data = vec![0; packed_len(new_bits)];
    for i in 0..CHUNK_VOLUME {
        let slot = remap[read_packed(data, bits, i)].expect("assigned in the first pass");
        write_packed(&mut new_data, new_bits, i, slot);
    }
    ChunkStorage::Paletted {
        palette: new_palette,
        bits: new_bits,
        data: new_data,
    }
}

/// xxh3 of a versioned little-endian encoding: prefix, palette length (u32),
/// palette ids (u16), bit width (u8), data words (u64).
fn canonical_hash(palette: &[MaterialId], bits: u8, data: &[u64]) -> u64 {
    const PREFIX: &[u8] = b"rmc.chunk.v1";
    let mut buf = Vec::with_capacity(PREFIX.len() + 4 + palette.len() * 2 + 1 + data.len() * 8);
    buf.extend_from_slice(PREFIX);
    buf.extend_from_slice(&(palette.len() as u32).to_le_bytes());
    for id in palette {
        buf.extend_from_slice(&id.0.to_le_bytes());
    }
    buf.push(bits);
    for word in data {
        buf.extend_from_slice(&word.to_le_bytes());
    }
    xxhash_rust::xxh3::xxh3_64(&buf)
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
    fn count_matches_per_cell_scan_at_every_width() {
        let mut s = ChunkStorage::Uniform(MaterialId(3));
        assert_eq!(s.count(MaterialId(3)), CHUNK_VOLUME);
        assert_eq!(s.count(MaterialId(4)), 0);
        // Grow through 1, 2, 4, 8 and 16 bits; at each width the count
        // equals a scan with `get`.
        let mut next = 10u16;
        for &width in &PALETTE_BITS {
            while !matches!(&s, ChunkStorage::Paletted { bits, .. } if *bits >= width) {
                for k in 0..7u32 {
                    let i = (u32::from(next) * 4099 + k * 977) % CHUNK_VOLUME as u32;
                    s.set(LocalPos::from_index(i as u16), MaterialId(next));
                }
                next += 1;
            }
            for id in [3, 10, next - 1, next, u16::MAX].map(MaterialId) {
                let scan = all_locals().filter(|&l| s.get(l) == id).count();
                assert_eq!(s.count(id), scan, "{id:?} at {width} bits");
            }
        }
        assert_eq!(s.count(MaterialId(next)), 0, "absent from the palette");
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

    /// Builds the same contents in a different palette order and at 16 bits:
    /// every cell starts as `fill`, then `cells` are written.
    fn build(fill: MaterialId, cells: &[(u16, MaterialId)]) -> ChunkStorage {
        let mut s = ChunkStorage::Uniform(fill);
        for &(i, id) in cells {
            s.set(LocalPos::from_index(i), id);
        }
        s
    }

    #[test]
    fn hash_uniform_equals_paletted() {
        let stone = MaterialId(4);
        let dirt = MaterialId(2);
        let uniform = ChunkStorage::Uniform(stone);

        // Promoted to a 1-bit palette, then written back: all stone again.
        let p = LocalPos::new(5, 6, 7);
        let mut reverted = uniform.clone();
        reverted.set(p, dirt);
        reverted.set(p, stone);
        assert!(matches!(reverted, ChunkStorage::Paletted { bits: 1, .. }));
        assert_eq!(reverted.hash(), uniform.hash());

        // Grown to 16 bits with 300 materials, then every one overwritten.
        let cells: Vec<u16> = (1u16..=300)
            .map(|n| (u32::from(n) * 109 % CHUNK_VOLUME as u32) as u16)
            .collect();
        let mut wide = uniform.clone();
        for (n, &i) in (1000u16..).zip(&cells) {
            wide.set(LocalPos::from_index(i), MaterialId(n));
        }
        assert!(matches!(wide, ChunkStorage::Paletted { bits: 16, .. }));
        for &i in &cells {
            wide.set(LocalPos::from_index(i), stone);
        }
        assert_eq!(wide.hash(), uniform.hash());

        // A chunk whose fill was entirely overwritten equals Uniform of the new material.
        let mut all_dirt = uniform.clone();
        for l in all_locals() {
            all_dirt.set(l, dirt);
        }
        assert!(matches!(all_dirt, ChunkStorage::Paletted { .. }));
        assert_eq!(all_dirt.hash(), ChunkStorage::Uniform(dirt).hash());

        // Same mixed contents, different palette order and bit width, compacted or not.
        let a = build(stone, &[(10, dirt), (20, MaterialId(9))]);
        let mut b = ChunkStorage::Uniform(MaterialId(9));
        for l in all_locals() {
            b.set(l, MaterialId(77)); // an entry that ends up unused
        }
        for l in all_locals() {
            let want = match l.index() {
                10 => dirt,
                20 => MaterialId(9),
                _ => stone,
            };
            b.set(l, want);
        }
        match (&a, &b) {
            (
                ChunkStorage::Paletted { palette: pa, .. },
                ChunkStorage::Paletted { palette: pb, .. },
            ) => assert_ne!(pa, pb, "fixture should differ structurally"),
            _ => panic!("fixtures should be paletted"),
        }
        assert_eq!(a.hash(), b.hash());
        let mut ac = a.clone();
        ac.compact();
        assert_eq!(ac.hash(), a.hash());

        // Chunk::hash covers contents only, not revision or modified.
        let mut c = Chunk::new(b.clone());
        c.revision = 42;
        c.modified = true;
        assert_eq!(c.hash(), a.hash());
        assert_eq!(Chunk::uniform(stone).hash(), uniform.hash());
    }

    #[test]
    fn hash_changes_on_edit() {
        let stone = MaterialId(4);
        let dirt = MaterialId(2);
        let base = ChunkStorage::Uniform(stone);
        let h0 = base.hash();

        assert_ne!(ChunkStorage::Uniform(dirt).hash(), h0);

        // One edited cell changes the hash, and where it is matters.
        let mut a = base.clone();
        a.set(LocalPos::new(0, 0, 0), dirt);
        let ha = a.hash();
        assert_ne!(ha, h0);
        let b = build(stone, &[(1, dirt)]);
        assert_ne!(b.hash(), ha);
        assert_ne!(b.hash(), h0);
        let last = build(stone, &[((CHUNK_VOLUME - 1) as u16, dirt)]);
        assert_ne!(last.hash(), ha);
        assert_ne!(last.hash(), h0);

        // Which material is written matters too.
        let c = build(stone, &[(0, MaterialId(9))]);
        assert_ne!(c.hash(), ha);

        // Swapping two cells with the same palette changes the hash.
        let x = build(stone, &[(100, dirt), (200, MaterialId(9))]);
        let y = build(stone, &[(100, MaterialId(9)), (200, dirt)]);
        assert_ne!(x.hash(), y.hash());

        // Each further edit changes it again; reverting restores it.
        let mut wide = build(
            stone,
            &(1u16..=300)
                .map(|n| {
                    (
                        (u32::from(n) * 109 % CHUNK_VOLUME as u32) as u16,
                        MaterialId(n),
                    )
                })
                .collect::<Vec<_>>(),
        );
        assert!(matches!(wide, ChunkStorage::Paletted { bits: 16, .. }));
        let hw = wide.hash();
        let p = LocalPos::from_index(31000);
        let prev = wide.set(p, MaterialId(5));
        assert_ne!(wide.hash(), hw);
        wide.set(p, prev);
        assert_eq!(wide.hash(), hw);

        a.set(LocalPos::new(0, 0, 0), stone);
        assert_eq!(a.hash(), h0);
    }
}
