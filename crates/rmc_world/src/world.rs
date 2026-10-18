//! The world (SAD §7): resident chunks inside fixed bounds, plus the material
//! table that gives cells their meaning.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::chunk::Chunk;
use crate::coords::{CellAabb, CellPos, ChunkPos};
use crate::edit::{
    ChunkDelta, EditCommand, EditResult, EditStatus, FULL_DELTA_THRESHOLD, MAX_EDIT_CELLS,
    RejectReason,
};
use crate::materials::{MaterialId, MaterialTable};

/// Errors from world mutation.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorldError {
    #[error("chunk {0:?} lies entirely outside the world bounds")]
    ChunkOutOfBounds(ChunkPos),
}

/// Chunks keyed by position, bounded by a [`CellAabb`].
#[derive(Clone, Debug)]
pub struct World {
    chunks: HashMap<ChunkPos, Chunk>,
    materials: Arc<MaterialTable>,
    bounds: CellAabb,
}

impl World {
    /// An empty world: no chunks are resident until inserted.
    pub fn new(materials: Arc<MaterialTable>, bounds: CellAabb) -> Self {
        Self {
            chunks: HashMap::new(),
            materials,
            bounds,
        }
    }

    pub fn bounds(&self) -> CellAabb {
        self.bounds
    }

    pub fn materials(&self) -> &Arc<MaterialTable> {
        &self.materials
    }

    /// Material at `cell`, or `None` if the cell is outside the bounds or its
    /// chunk is not resident.
    pub fn get_cell(&self, cell: CellPos) -> Option<MaterialId> {
        if !self.bounds.contains(cell) {
            return None;
        }
        let (pos, local) = cell.split();
        self.chunks.get(&pos).map(|c| c.storage.get(local))
    }

    /// Whether `cell` holds a material whose phase is `Solid`. Cells outside
    /// the bounds, in a missing chunk, or with an id the table does not know
    /// are not solid.
    pub fn is_solid(&self, cell: CellPos) -> bool {
        self.get_cell(cell)
            .and_then(|id| self.materials.get(id))
            .is_some_and(|def| def.is_solid())
    }

    /// The resident chunk at `pos`.
    pub fn chunk(&self, pos: ChunkPos) -> Option<&Chunk> {
        self.chunks.get(&pos)
    }

    /// Number of resident chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Approximate bytes the chunks occupy: the `World` itself, the chunk
    /// map's table, and each chunk's heap storage. The shared material table
    /// is not counted.
    ///
    /// The table size follows std's SwissTable layout: `capacity()` usable
    /// slots in a power-of-two bucket count at 7/8 load, one entry and one
    /// control byte per bucket, plus a 16-byte control group.
    pub fn memory_bytes(&self) -> usize {
        let cap = self.chunks.capacity();
        let buckets = match cap {
            0 => 0,
            1..7 => cap + 1,
            _ => cap / 7 * 8,
        };
        let table = if buckets == 0 {
            0
        } else {
            buckets * (std::mem::size_of::<(ChunkPos, Chunk)>() + 1) + 16
        };
        let cells: usize = self.chunks.values().map(|c| c.storage.heap_bytes()).sum();
        std::mem::size_of::<Self>() + table + cells
    }

    /// Resident chunks in [`ChunkPos`] order (y, then z, then x). Use this for
    /// anything whose output depends on iteration order.
    pub fn iter_chunks_sorted(&self) -> impl Iterator<Item = (ChunkPos, &Chunk)> {
        let mut sorted: Vec<(ChunkPos, &Chunk)> =
            self.chunks.iter().map(|(p, c)| (*p, c)).collect();
        sorted.sort_unstable_by_key(|(p, _)| *p);
        sorted.into_iter()
    }

    /// xxh3 folding `(pos, chunk.hash())` over [`Self::iter_chunks_sorted`].
    /// Revisions, `modified`, bounds and the material table are not hashed.
    pub fn hash(&self) -> u64 {
        const PREFIX: &[u8] = b"rmc.world.v1";
        let mut buf = Vec::with_capacity(PREFIX.len() + self.chunks.len() * 20);
        buf.extend_from_slice(PREFIX);
        for (pos, chunk) in self.iter_chunks_sorted() {
            for v in pos.0.to_array() {
                buf.extend_from_slice(&v.to_le_bytes());
            }
            buf.extend_from_slice(&chunk.hash().to_le_bytes());
        }
        xxhash_rust::xxh3::xxh3_64(&buf)
    }

    /// Whole-command checks (SAD §8.2). A command that fails any of them is
    /// rejected by [`World::apply`] with nothing applied. In order: the op
    /// must name a known material (and be supported), the shape must have at
    /// most [`MAX_EDIT_CELLS`] cells, and at least one of its cells must lie
    /// inside the bounds. Resident chunks do not matter here.
    pub fn validate(&self, cmd: &EditCommand) -> Result<(), RejectReason> {
        cmd.op.validate(&self.materials)?;
        if cmd.shape.cell_count_exceeds(MAX_EDIT_CELLS) {
            return Err(RejectReason::TooManyCells);
        }
        // At most MAX_EDIT_CELLS cells to scan, and it stops at the first hit.
        if !cmd.shape.cells().any(|c| self.bounds.contains(c)) {
            return Err(RejectReason::OutsideBounds);
        }
        Ok(())
    }

    /// Applies an edit command (SAD §8.1–8.3) and returns the deltas a
    /// replica needs to reproduce it.
    ///
    /// The command is first checked by [`World::validate`]; if that fails it
    /// is rejected whole and nothing changes. Cells are then visited in shape
    /// order. A cell is skipped when it is outside the bounds, its chunk is
    /// not resident, the op does not apply to its material, or it already
    /// holds the target. Each changed chunk gets one
    /// revision bump and is marked modified; its delta is `Full` (with the
    /// chunk compacted) when more than [`FULL_DELTA_THRESHOLD`] cells changed.
    pub fn apply(&mut self, cmd: &EditCommand) -> EditResult {
        if let Err(reason) = self.validate(cmd) {
            tracing::debug!(command = ?cmd.id, %reason, "edit rejected");
            return EditResult::rejected(cmd.id, reason);
        }

        let mut changed: BTreeMap<ChunkPos, Vec<(u16, MaterialId)>> = BTreeMap::new();
        for cell in cmd.shape.cells() {
            if !self.bounds.contains(cell) {
                continue;
            }
            let (pos, local) = cell.split();
            let Some(chunk) = self.chunks.get_mut(&pos) else {
                continue;
            };
            let old = chunk.storage.get(local);
            let Some(new) = cmd.op.target(self.materials.get(old)) else {
                continue;
            };
            if new != old {
                chunk.storage.set(local, new);
                changed.entry(pos).or_default().push((local.index(), new));
            }
        }

        let deltas = changed
            .into_iter()
            .map(|(pos, mut changes)| {
                let chunk = self
                    .chunks
                    .get_mut(&pos)
                    .expect("changed chunk is resident");
                chunk.revision += 1;
                chunk.modified = true;
                let revision = chunk.revision;
                if changes.len() > FULL_DELTA_THRESHOLD {
                    chunk.storage.compact();
                    ChunkDelta::Full {
                        pos,
                        revision,
                        storage: chunk.storage.clone(),
                    }
                } else {
                    // Shape order is (y, z, x) ascending, which within one
                    // chunk is already ascending local index; sort anyway so
                    // the delta does not depend on that.
                    changes.sort_unstable_by_key(|&(i, _)| i);
                    ChunkDelta::Cells {
                        pos,
                        revision,
                        changes,
                    }
                }
            })
            .collect();

        EditResult {
            command: cmd.id,
            status: EditStatus::Applied,
            deltas,
            yields: Vec::new(),
        }
    }

    /// Makes `chunk` resident at `pos`, replacing and returning any previous
    /// chunk there. Rejects a chunk with no cell inside the bounds.
    pub fn insert_chunk(
        &mut self,
        pos: ChunkPos,
        chunk: Chunk,
    ) -> Result<Option<Chunk>, WorldError> {
        if !self.bounds.intersects_chunk(pos) {
            return Err(WorldError::ChunkOutOfBounds(pos));
        }
        tracing::debug!(?pos, revision = chunk.revision, "chunk inserted");
        Ok(self.chunks.insert(pos, chunk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkStorage;
    use crate::coords::{CHUNK_SIZE, LocalPos};
    use proptest::prelude::*;

    const PHASE1_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    fn phase1() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(PHASE1_PATH).expect("assets/materials.ron loads"))
    }

    fn id(table: &MaterialTable, key: &str) -> MaterialId {
        table
            .id(key)
            .unwrap_or_else(|| panic!("material {key} in table"))
    }

    /// A small world: 3 × 2 × 3 chunks, not starting at the origin, so
    /// negative coordinates are in bounds.
    fn small_bounds() -> CellAabb {
        CellAabb::new(CellPos::new(-32, -32, -32), CellPos::new(63, 31, 63))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(
            std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(256)
        ))]

        /// Any insertion order gives the same strictly ascending (y, z, x)
        /// sequence of chunks.
        #[test]
        fn iter_sorted_is_deterministic(
            order in Just(small_bounds().chunks().collect::<Vec<_>>()).prop_shuffle(),
            subset in proptest::collection::vec(any::<bool>(), 18),
        ) {
            let table = phase1();
            let stone = id(&table, "stone");
            let all: Vec<ChunkPos> = small_bounds().chunks().collect();
            prop_assert_eq!(all.len(), 18);
            let chosen: Vec<ChunkPos> = all
                .iter()
                .zip(&subset)
                .filter(|(_, keep)| **keep)
                .map(|(p, _)| *p)
                .collect();

            let mut a = World::new(table.clone(), small_bounds());
            let mut b = World::new(table, small_bounds());
            for &pos in all.iter().filter(|p| chosen.contains(p)) {
                a.insert_chunk(pos, Chunk::uniform(stone)).unwrap();
            }
            for &pos in order.iter().filter(|p| chosen.contains(p)) {
                b.insert_chunk(pos, Chunk::uniform(stone)).unwrap();
            }

            let seq_a: Vec<ChunkPos> = a.iter_chunks_sorted().map(|(p, _)| p).collect();
            let seq_b: Vec<ChunkPos> = b.iter_chunks_sorted().map(|(p, _)| p).collect();
            prop_assert_eq!(&seq_a, &seq_b);
            prop_assert_eq!(seq_a.len(), chosen.len());
            for w in seq_a.windows(2) {
                let (p, q) = (w[0].0, w[1].0);
                prop_assert!((p.y, p.z, p.x) < (q.y, q.z, q.x), "{p:?} before {q:?}");
            }
            // Iterating twice gives the same sequence.
            let again: Vec<ChunkPos> = a.iter_chunks_sorted().map(|(p, _)| p).collect();
            prop_assert_eq!(seq_a, again);
        }
    }

    #[test]
    fn iter_sorted_orders_y_then_z_then_x() {
        let table = phase1();
        let stone = id(&table, "stone");
        let mut w = World::new(table, small_bounds());
        for pos in small_bounds()
            .chunks()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            w.insert_chunk(pos, Chunk::uniform(stone)).unwrap();
        }
        let seq: Vec<ChunkPos> = w.iter_chunks_sorted().map(|(p, _)| p).collect();
        assert_eq!(seq.first(), Some(&ChunkPos::new(-1, -1, -1)));
        assert_eq!(seq[1], ChunkPos::new(0, -1, -1));
        assert_eq!(seq[3], ChunkPos::new(-1, -1, 0));
        assert_eq!(seq[9], ChunkPos::new(-1, 0, -1));
        assert_eq!(seq.last(), Some(&ChunkPos::new(1, 0, 1)));
    }

    #[test]
    fn get_cell_out_of_bounds_is_none() {
        let table = phase1();
        let stone = id(&table, "stone");
        let air = MaterialId::AIR;
        let b = CellAabb::PHASE1_DEFAULT;
        let mut w = World::new(table, b);
        for pos in b.chunks() {
            w.insert_chunk(pos, Chunk::uniform(stone)).unwrap();
        }
        assert_eq!(w.chunk_count(), 256);

        // The eight corners are inside.
        for &x in &[0, 255] {
            for &y in &[0, 127] {
                for &z in &[0, 255] {
                    let c = CellPos::new(x, y, z);
                    assert_eq!(w.get_cell(c), Some(stone), "{c:?}");
                    assert!(w.is_solid(c), "{c:?}");
                }
            }
        }
        // One step past each face is outside.
        for c in [
            CellPos::new(-1, 0, 0),
            CellPos::new(256, 0, 0),
            CellPos::new(0, -1, 0),
            CellPos::new(0, 128, 0),
            CellPos::new(0, 0, -1),
            CellPos::new(0, 0, 256),
            CellPos::new(i32::MIN, i32::MIN, i32::MIN),
            CellPos::new(i32::MAX, i32::MAX, i32::MAX),
        ] {
            assert_eq!(w.get_cell(c), None, "{c:?}");
            assert!(!w.is_solid(c), "{c:?}");
        }

        // A chunk that straddles the bounds: cells past the bound are None
        // even though the chunk is resident.
        let tall = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 40, 31));
        let mut w = World::new(phase1(), tall);
        w.insert_chunk(ChunkPos::new(0, 1, 0), Chunk::uniform(stone))
            .unwrap();
        assert_eq!(w.get_cell(CellPos::new(0, 40, 0)), Some(stone));
        assert_eq!(w.get_cell(CellPos::new(0, 41, 0)), None);
        assert!(!w.is_solid(CellPos::new(0, 41, 0)));

        // In bounds but not resident.
        assert_eq!(w.get_cell(CellPos::new(0, 0, 0)), None);
        assert!(!w.is_solid(CellPos::new(0, 0, 0)));

        // Air reads back as Some(AIR) and is not solid; an edited cell reads back.
        let mut chunk = Chunk::uniform(air);
        chunk.storage.set(LocalPos::new(3, 4, 5), stone);
        w.insert_chunk(ChunkPos::new(0, 0, 0), chunk).unwrap();
        assert_eq!(w.get_cell(CellPos::new(0, 0, 0)), Some(air));
        assert!(!w.is_solid(CellPos::new(0, 0, 0)));
        assert_eq!(w.get_cell(CellPos::new(3, 4, 5)), Some(stone));
        assert!(w.is_solid(CellPos::new(3, 4, 5)));

        // Chunks wholly outside the bounds are rejected and not stored.
        for pos in [
            ChunkPos::new(1, 0, 0),
            ChunkPos::new(0, 2, 0),
            ChunkPos::new(-1, 0, 0),
        ] {
            assert_eq!(
                w.insert_chunk(pos, Chunk::uniform(stone)),
                Err(WorldError::ChunkOutOfBounds(pos))
            );
            assert!(w.chunk(pos).is_none());
        }
        // Replacing returns the previous chunk.
        let prev = w
            .insert_chunk(ChunkPos::new(0, 0, 0), Chunk::uniform(stone))
            .unwrap();
        assert_eq!(
            prev.map(|c| c.storage.get(LocalPos::new(3, 4, 5))),
            Some(stone)
        );
        assert_eq!(w.get_cell(CellPos::new(0, 0, 0)), Some(stone));
    }

    #[test]
    fn world_hash_independent_of_insert_order() {
        let table = phase1();
        let stone = id(&table, "stone");
        let granite = id(&table, "granite");
        let positions: Vec<ChunkPos> = small_bounds().chunks().collect();
        let chunk_for = |pos: ChunkPos| {
            // Different contents per chunk so a mix-up would show.
            let mut c = Chunk::uniform(stone);
            let n = (pos.0.x + 1 + 3 * (pos.0.z + 1) + 9 * (pos.0.y + 1)) as u32;
            c.storage
                .set(LocalPos::new(n % CHUNK_SIZE as u32, 0, 0), granite);
            c
        };

        let mut forward = World::new(table.clone(), small_bounds());
        for &p in &positions {
            forward.insert_chunk(p, chunk_for(p)).unwrap();
        }
        let mut backward = World::new(table.clone(), small_bounds());
        for &p in positions.iter().rev() {
            backward.insert_chunk(p, chunk_for(p)).unwrap();
        }
        // An interleaved order, with revisions and `modified` that differ.
        let mut shuffled = World::new(table.clone(), small_bounds());
        for i in (0..positions.len()).map(|i| (i * 7) % positions.len()) {
            let p = positions[i];
            let mut c = chunk_for(p);
            c.revision = i as u64 * 3;
            c.modified = i % 2 == 0;
            shuffled.insert_chunk(p, c).unwrap();
        }
        assert_eq!(shuffled.chunk_count(), positions.len());
        assert_eq!(forward.hash(), backward.hash());
        assert_eq!(forward.hash(), shuffled.hash());

        // An equivalent but structurally different chunk keeps the hash.
        let p0 = positions[0];
        let mut promoted = chunk_for(p0);
        promoted.storage.set(LocalPos::new(31, 31, 31), granite);
        promoted.storage.set(LocalPos::new(31, 31, 31), stone);
        assert!(matches!(promoted.storage, ChunkStorage::Paletted { .. }));
        let mut equiv = forward.clone();
        equiv.insert_chunk(p0, promoted).unwrap();
        assert_eq!(equiv.hash(), forward.hash());

        // One changed cell changes the hash.
        let mut edited = forward.clone();
        let mut c = chunk_for(p0);
        c.storage.set(LocalPos::new(10, 10, 10), granite);
        edited.insert_chunk(p0, c).unwrap();
        assert_ne!(edited.hash(), forward.hash());

        // Swapping two chunks' positions changes the hash.
        let (p1, p2) = (positions[1], positions[2]);
        let mut swapped = forward.clone();
        swapped.insert_chunk(p1, chunk_for(p2)).unwrap();
        swapped.insert_chunk(p2, chunk_for(p1)).unwrap();
        assert_ne!(swapped.hash(), forward.hash());

        // A missing chunk changes the hash; an empty world hashes stably.
        let mut fewer = World::new(table.clone(), small_bounds());
        for &p in &positions[1..] {
            fewer.insert_chunk(p, chunk_for(p)).unwrap();
        }
        assert_ne!(fewer.hash(), forward.hash());
        let empty_a = World::new(table.clone(), small_bounds());
        let empty_b = World::new(table, CellAabb::PHASE1_DEFAULT);
        assert_eq!(empty_a.hash(), empty_b.hash());
        assert_ne!(empty_a.hash(), forward.hash());
    }

    /// SAD §13 budget: the Phase 1 world of 256 chunks fits in 64 MB, both
    /// as uniform stone (the worldgen best case) and with every chunk at the
    /// widest 16-bit palette (the worst case).
    #[test]
    fn memory_256_chunks_under_budget() {
        const BUDGET: usize = 64_000_000;
        let table = phase1();
        let stone = id(&table, "stone");
        let b = CellAabb::PHASE1_DEFAULT;

        let mut w = World::new(table.clone(), b);
        assert_eq!(w.memory_bytes(), std::mem::size_of::<World>());
        for pos in b.chunks() {
            w.insert_chunk(pos, Chunk::uniform(stone)).unwrap();
        }
        assert_eq!(w.chunk_count(), 256);
        let uniform = w.memory_bytes();
        assert!(uniform < BUDGET, "uniform world uses {uniform} bytes");
        // The map holds at least the 256 entries, and nothing else is on the heap.
        assert!(uniform >= 256 * std::mem::size_of::<(ChunkPos, Chunk)>());
        assert!(
            uniform < 1 << 20,
            "uniform chunks own no cell data: {uniform}"
        );
        for (_, c) in w.iter_chunks_sorted() {
            assert_eq!(c.storage.heap_bytes(), 0);
        }

        // Worst case: 257 distinct ids force 16 bits per cell.
        let mut wide = Chunk::uniform(MaterialId::AIR);
        for i in 0..257u16 {
            let n = u32::from(i);
            wide.storage.set(
                LocalPos::new(n % 32, n / 1024, (n / 32) % 32),
                MaterialId(i),
            );
        }
        let ChunkStorage::Paletted { bits, .. } = &wide.storage else {
            panic!("257 materials promote to Paletted");
        };
        assert_eq!(*bits, 16);
        let wide_heap = wide.storage.heap_bytes();
        // 32768 cells × 2 bytes of data, plus the palette.
        assert!(wide_heap >= 32768 * 2 + 257 * 2, "{wide_heap}");
        for pos in b.chunks() {
            w.insert_chunk(pos, wide.clone()).unwrap();
        }
        assert_eq!(w.chunk_count(), 256);
        let worst = w.memory_bytes();
        assert!(worst >= 256 * 32768 * 2, "{worst}");
        assert!(worst < BUDGET, "16-bit world uses {worst} bytes");
        assert!(worst - uniform >= 256 * 32768 * 2);
        eprintln!("256 chunks: uniform {uniform} bytes, 16-bit {worst} bytes");
    }
}
