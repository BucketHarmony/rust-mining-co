//! The world (SAD §7): resident chunks inside fixed bounds, plus the material
//! table that gives cells their meaning.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use crate::chunk::{Chunk, ChunkStorage};
use crate::columns::{Columns, NO_SURFACE};
use crate::coords::{CHUNK_VOLUME, CellAabb, CellPos, ChunkPos, LocalPos};
use crate::damage::DamageMap;
use crate::displace::{Cause, EcoRules, Effects, layer_change_ok};
use crate::edit::{
    ChunkDelta, EditCommand, EditOp, EditResult, EditStatus, FULL_DELTA_THRESHOLD, MAX_EDIT_CELLS,
    RejectReason,
};
use crate::layers::{ChunkLayers, LayerId, PlantState};
use crate::ledger::SeasonLedger;
use crate::materials::{MaterialDef, MaterialId, MaterialTable, Phase};
use crate::shape::Shape;

/// Errors from world mutation.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorldError {
    #[error("chunk {0:?} lies entirely outside the world bounds")]
    ChunkOutOfBounds(ChunkPos),
}

/// Why [`World::apply_delta`] refused a delta. A refused delta changes nothing.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DeltaError {
    #[error("chunk {0:?} lies entirely outside the world bounds")]
    OutOfBounds(ChunkPos),
    #[error("chunk {0:?} is not resident")]
    NotResident(ChunkPos),
    #[error("revision gap in chunk {pos:?}: local revision {local}, delta revision {got}")]
    RevisionGap { pos: ChunkPos, local: u64, got: u64 },
    #[error("cell index {index} in chunk {pos:?} is outside the chunk")]
    BadIndex { pos: ChunkPos, index: u16 },
    #[error("material {id:?} in chunk {pos:?} is not in the material table")]
    UnknownMaterial { pos: ChunkPos, id: MaterialId },
    #[error("storage for chunk {0:?} is malformed")]
    MalformedStorage(ChunkPos),
    #[error("layer change ({index}, {layer:?}, {value}) in chunk {pos:?} is invalid")]
    BadLayerChange {
        pos: ChunkPos,
        index: u16,
        layer: LayerId,
        value: i32,
    },
}

/// Chunks keyed by position, bounded by a [`CellAabb`].
#[derive(Clone, Debug)]
pub struct World {
    pub(crate) chunks: HashMap<ChunkPos, Chunk>,
    pub(crate) damage: DamageMap,
    pub(crate) materials: Arc<MaterialTable>,
    pub(crate) bounds: CellAabb,
    pub(crate) columns: Columns,
    /// Roles for the displacement rules (SAD-P2 §7.0).
    pub(crate) rules: Arc<EcoRules>,
}

impl World {
    /// An empty world: no chunks are resident until inserted.
    ///
    /// Panics unless every y in `bounds` fits `i16` above [`NO_SURFACE`],
    /// the range `surface_y` stores (SAD-P2 §6).
    pub fn new(materials: Arc<MaterialTable>, bounds: CellAabb) -> Self {
        assert!(
            bounds.min.0.y > i32::from(NO_SURFACE) && bounds.max.0.y <= i32::from(i16::MAX),
            "world y range {}..={} does not fit i16",
            bounds.min.0.y,
            bounds.max.0.y
        );
        Self {
            chunks: HashMap::new(),
            damage: DamageMap::default(),
            materials,
            bounds,
            columns: Columns::default(),
            rules: Arc::default(),
        }
    }

    pub fn bounds(&self) -> CellAabb {
        self.bounds
    }

    pub fn materials(&self) -> &Arc<MaterialTable> {
        &self.materials
    }

    /// Accumulated cell damage. Host-only: not hashed, saved or sent.
    pub fn damage(&self) -> &DamageMap {
        &self.damage
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

    /// Whether `cell` holds a `Solid` or `Liquid` material: what raycasts
    /// hit (SAD-P2 §5). Same out-of-bounds rules as [`World::is_solid`].
    pub fn is_solid_or_liquid(&self, cell: CellPos) -> bool {
        self.get_cell(cell)
            .and_then(|id| self.materials.get(id))
            .is_some_and(|def| def.is_solid_or_liquid())
    }

    /// `surface_y` of the cell column `(x, z)` (SAD-P2 §3): the highest
    /// in-bounds resident cell that is `Liquid`, or `Solid` and not
    /// `passable`; [`NO_SURFACE`] if there is none. "Sky-open" is
    /// `y > surface_y`.
    pub fn surface_y(&self, x: i32, z: i32) -> i16 {
        self.columns.surface_y(x, z)
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
    /// map's table, and each chunk's heap data ([`Chunk::heap_bytes`]). The
    /// shared material table and the `surface_y` cache are not counted.
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
        let chunks: usize = self.chunks.values().map(Chunk::heap_bytes).sum();
        std::mem::size_of::<Self>() + table + chunks
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
    /// inside the bounds. Resident chunks do not matter here, except that a
    /// `Place` must have a [`Shape::Cell`] shape whose cell is not `Solid`.
    pub fn validate(&self, cmd: &EditCommand) -> Result<(), RejectReason> {
        cmd.op.validate(&self.materials)?;
        if cmd.shape.cell_count_exceeds(MAX_EDIT_CELLS) {
            return Err(RejectReason::TooManyCells);
        }
        // At most MAX_EDIT_CELLS cells to scan, and it stops at the first hit.
        if !cmd.shape.cells().any(|c| self.bounds.contains(c)) {
            return Err(RejectReason::OutsideBounds);
        }
        if let EditOp::Place(_) = cmd.op {
            let Shape::Cell(cell) = cmd.shape else {
                return Err(RejectReason::PlaceNeedsCell);
            };
            let solid = self
                .get_cell(cell)
                .and_then(|id| self.materials.get(id))
                .is_some_and(MaterialDef::is_solid);
            if solid {
                return Err(RejectReason::PlaceOnSolid(cell));
            }
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
    ///
    /// `Damage` adds to each diggable cell's damage (saturating). A cell whose
    /// damage reaches its hardness breaks: it becomes air and its material's
    /// `yield_count` is added to the yields. Damage without a break changes
    /// no cell and produces no delta. `Set`, `Paint` and `Remove` clear the
    /// damage on every cell they apply to.
    pub fn apply(&mut self, cmd: &EditCommand) -> EditResult {
        self.apply_keeping_clear(cmd, &[])
    }

    /// [`World::apply`], except that a `Set` or `Place` also skips every cell inside
    /// one of the `keep_clear` boxes (the host's player boxes), cell by cell
    /// like the other skips: the rest of the command applies, and a skipped
    /// cell keeps its material and damage. The other ops ignore the boxes.
    pub fn apply_keeping_clear(
        &mut self,
        cmd: &EditCommand,
        keep_clear: &[CellAabb],
    ) -> EditResult {
        if let Err(reason) = self.validate(cmd) {
            tracing::debug!(command = ?cmd.id, %reason, "edit rejected");
            return EditResult::rejected(cmd.id, reason);
        }

        let mut ledger = SeasonLedger::default();
        let mut fx = Effects::default();
        let mut yields: BTreeMap<MaterialId, u32> = BTreeMap::new();
        let cause = Cause::Edit(cmd.op);
        for cell in cmd.shape.cells() {
            if !self.bounds.contains(cell) {
                continue;
            }
            let (pos, local) = cell.split();
            let Some(chunk) = self.chunks.get(&pos) else {
                continue;
            };
            let old = chunk.storage.get(local);
            let def = self.materials.get(old);
            let new = match cmd.op {
                EditOp::SetLayer { layer, value } => {
                    if matches!(layer, LayerId::Gas(_)) && def.is_none_or(|d| d.phase != Phase::Air)
                    {
                        continue;
                    }
                    self.set_layer_by_edit(cell, layer, value, &mut ledger, &mut fx);
                    continue;
                }
                EditOp::Damage(dmg) => {
                    let Some(def) = def.filter(|d| d.diggable) else {
                        continue;
                    };
                    if !self.damage.hit(cell, def.hardness, dmg) {
                        continue;
                    }
                    if def.yield_count > 0 {
                        *yields.entry(old).or_insert(0) += u32::from(def.yield_count);
                    }
                    MaterialId::AIR
                }
                _ => {
                    if matches!(cmd.op, EditOp::Set(_) | EditOp::Place(_))
                        && keep_clear.iter().any(|b| b.contains(cell))
                    {
                        continue;
                    }
                    let Some(new) = cmd.op.target(def) else {
                        continue;
                    };
                    // Set, Paint and Remove clear damage on every cell they
                    // apply to, even one that already holds the target.
                    self.damage.clear(cell);
                    new
                }
            };
            self.change_material(cell, new, cause, &mut ledger, &mut fx);
        }
        self.finish_effects(&fx);
        for (&m, &n) in &fx.yields {
            *yields.entry(m).or_insert(0) += n;
        }

        let chunks: BTreeSet<ChunkPos> = fx
            .cells
            .keys()
            .chain(fx.layers.keys())
            .chain(fx.plants.keys())
            .copied()
            .collect();
        let mut deltas = Vec::new();
        for pos in chunks {
            let chunk = self
                .chunks
                .get_mut(&pos)
                .expect("changed chunk is resident");
            if let Some(changes) = fx.cells.get(&pos) {
                let revision = chunk.revision;
                deltas.push(if changes.len() > FULL_DELTA_THRESHOLD {
                    chunk.storage.compact();
                    ChunkDelta::Full {
                        pos,
                        revision,
                        storage: chunk.storage.clone(),
                    }
                } else {
                    ChunkDelta::Cells {
                        pos,
                        revision,
                        changes: changes.iter().map(|(&i, &m)| (i, m)).collect(),
                    }
                });
            }
            let layers = fx.layers.get(&pos);
            let plants = fx.plants.get(&pos);
            if layers.is_some() || plants.is_some() {
                deltas.push(ChunkDelta::Layers {
                    pos,
                    eco_revision: chunk.eco_revision,
                    changes: layers
                        .into_iter()
                        .flatten()
                        .map(|(&(i, id), &v)| (i, id, v))
                        .collect(),
                    plants: plants
                        .into_iter()
                        .flatten()
                        .map(|(&i, p)| (i, *p))
                        .collect(),
                });
            }
        }

        EditResult {
            command: cmd.id,
            status: EditStatus::Applied,
            deltas,
            yields: yields.into_iter().collect(),
            ledger: Box::new(ledger),
        }
    }

    /// `SetLayer` on one cell: writes the value and ledgers the change of a
    /// conserved quantity as an edit source or sink.
    fn set_layer_by_edit(
        &mut self,
        cell: CellPos,
        layer: LayerId,
        value: i32,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) {
        let diff = i64::from(value - self.layer(cell, layer));
        if diff == 0 {
            return;
        }
        self.put_layer(cell, layer, value, fx);
        let (gain, loss) = (diff.max(0), (-diff).max(0));
        match layer {
            LayerId::Moisture => {
                ledger.water_edit_in += gain;
                ledger.water_edit_out += loss;
            }
            LayerId::Substance(x) => {
                ledger.edit_in[usize::from(x)] += gain;
                ledger.edit_out[usize::from(x)] += loss;
            }
            LayerId::Gas(g) => {
                ledger.gas_edit_in[usize::from(g)] += gain;
                ledger.gas_edit_out[usize::from(g)] += loss;
                if self.rules.steam == Some(g) {
                    ledger.water_edit_in += gain;
                    ledger.water_edit_out += loss;
                }
            }
            LayerId::Temperature | LayerId::Regolith | LayerId::Light | LayerId::Growth => {}
        }
    }

    /// Applies a delta produced by [`World::apply`] on another world (SAD §7,
    /// §8.5). The whole delta is checked before anything is written, so a
    /// refused delta changes nothing.
    ///
    /// `Cells` needs a resident chunk whose revision is exactly one below
    /// the delta's; anything else is a [`DeltaError::RevisionGap`], which a
    /// replica repairs with `RequestChunk`. Its indices must lie inside the
    /// chunk and its materials in the table. Changes are written in order.
    ///
    /// `Full` replaces the chunk's cells whatever its local revision, and
    /// makes an in-bounds chunk resident if it was not. Its storage must be
    /// well formed and every material it uses must be in the table; it is
    /// stored compacted. A resident chunk keeps its layers and plants.
    ///
    /// Both kinds keep `surface_y` current for the columns they touch.
    ///
    /// Either way the chunk takes the delta's revision and is marked
    /// modified (as it is on the world that produced the delta), and damage
    /// on the overwritten cells is cleared.
    ///
    /// `Layers` needs a resident chunk whose `eco_revision` is exactly one
    /// below the delta's (a gap is a [`DeltaError::RevisionGap`] on the eco
    /// revision, repaired the same way). Every change must name a cell in
    /// the chunk, a valid layer and a value of its type. Values and plant
    /// states are written in order; cells, `revision`, `modified`, damage
    /// and `surface_y` are untouched.
    pub fn apply_delta(&mut self, delta: &ChunkDelta) -> Result<(), DeltaError> {
        let pos = delta.pos();
        if !self.bounds.intersects_chunk(pos) {
            return Err(DeltaError::OutOfBounds(pos));
        }
        let known = |id: MaterialId| {
            if self.materials.get(id).is_some() {
                Ok(())
            } else {
                Err(DeltaError::UnknownMaterial { pos, id })
            }
        };
        match delta {
            ChunkDelta::Cells {
                revision, changes, ..
            } => {
                let chunk = self.chunks.get(&pos).ok_or(DeltaError::NotResident(pos))?;
                if chunk.revision.checked_add(1) != Some(*revision) {
                    return Err(DeltaError::RevisionGap {
                        pos,
                        local: chunk.revision,
                        got: *revision,
                    });
                }
                for &(index, id) in changes {
                    if usize::from(index) >= CHUNK_VOLUME {
                        return Err(DeltaError::BadIndex { pos, index });
                    }
                    known(id)?;
                }
                for &(index, id) in changes {
                    let local = LocalPos::from_index(index);
                    let cell = CellPos::from_chunk_local(pos, local);
                    let chunk = self.chunks.get_mut(&pos).expect("checked resident");
                    let old = chunk.storage.set(local, id);
                    self.damage.clear(cell);
                    self.columns.cell_changed(
                        cell,
                        old,
                        id,
                        &self.chunks,
                        &self.materials,
                        self.bounds,
                    );
                }
                let chunk = self.chunks.get_mut(&pos).expect("checked resident");
                chunk.revision = *revision;
                chunk.modified = true;
            }
            ChunkDelta::Full {
                revision, storage, ..
            } => {
                if !storage.is_well_formed() {
                    return Err(DeltaError::MalformedStorage(pos));
                }
                let mut storage = storage.clone();
                storage.compact();
                match &storage {
                    ChunkStorage::Uniform(id) => known(*id)?,
                    // Compacted, so the palette holds only materials in use.
                    ChunkStorage::Paletted { palette, .. } => {
                        palette.iter().try_for_each(|&id| known(id))?
                    }
                }
                self.damage.clear_chunk(pos);
                // Only the cells are replaced: a resident chunk keeps its
                // layers, plants and eco revision.
                let chunk = self
                    .chunks
                    .entry(pos)
                    .or_insert_with(|| Chunk::new(ChunkStorage::Uniform(MaterialId::AIR)));
                chunk.storage = storage;
                chunk.revision = *revision;
                chunk.modified = true;
                self.rebuild_columns(pos);
            }
            ChunkDelta::Layers {
                eco_revision,
                changes,
                plants,
                ..
            } => {
                let chunk = self.chunks.get(&pos).ok_or(DeltaError::NotResident(pos))?;
                if chunk.eco_revision.checked_add(1) != Some(*eco_revision) {
                    return Err(DeltaError::RevisionGap {
                        pos,
                        local: chunk.eco_revision,
                        got: *eco_revision,
                    });
                }
                if let Some(&(index, layer, value)) = changes
                    .iter()
                    .find(|&&(i, id, v)| !layer_change_ok(i, id, v))
                {
                    return Err(DeltaError::BadLayerChange {
                        pos,
                        index,
                        layer,
                        value,
                    });
                }
                if let Some(&(index, _)) =
                    plants.iter().find(|(i, _)| usize::from(*i) >= CHUNK_VOLUME)
                {
                    return Err(DeltaError::BadIndex { pos, index });
                }
                let chunk = self.chunks.get_mut(&pos).expect("checked resident");
                for &(index, layer, value) in changes {
                    let local = LocalPos::from_index(index);
                    if layer.is_i16() {
                        chunk
                            .layers
                            .set_i16(layer, local, i16::try_from(value).expect("checked"));
                    } else {
                        chunk
                            .layers
                            .set_u8(layer, local, u8::try_from(value).expect("checked"));
                    }
                }
                for &(index, plant) in plants {
                    match plant {
                        Some(p) => chunk.plants.insert(index, p),
                        None => chunk.plants.remove(&index),
                    };
                }
                chunk.eco_revision = *eco_revision;
            }
        }
        tracing::debug!(?pos, revision = delta.revision(), "delta applied");
        Ok(())
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
        self.damage.clear_chunk(pos);
        let previous = self.chunks.insert(pos, chunk);
        self.rebuild_columns(pos);
        Ok(previous)
    }

    /// Replaces the ecology state of the resident chunk at `pos` (snapshot
    /// application: a save load, or a layer snapshot): its layers and plants
    /// become `layers` and `plants` and its eco revision `eco_revision`.
    /// Cells, revision, `modified` and `surface_y` are untouched, since
    /// layers do not change materials. A refused snapshot changes nothing.
    pub fn apply_eco_snapshot(
        &mut self,
        pos: ChunkPos,
        eco_revision: u64,
        layers: ChunkLayers,
        plants: BTreeMap<u16, PlantState>,
    ) -> Result<(), DeltaError> {
        if !self.bounds.intersects_chunk(pos) {
            return Err(DeltaError::OutOfBounds(pos));
        }
        if let Some(&index) = plants.keys().find(|&&i| usize::from(i) >= CHUNK_VOLUME) {
            return Err(DeltaError::BadIndex { pos, index });
        }
        let chunk = self
            .chunks
            .get_mut(&pos)
            .ok_or(DeltaError::NotResident(pos))?;
        chunk.layers = layers;
        chunk.plants = plants;
        chunk.eco_revision = eco_revision;
        tracing::debug!(?pos, eco_revision, "eco snapshot applied");
        Ok(())
    }

    /// Recomputes `surface_y` for every cell column of `pos`'s chunk column.
    fn rebuild_columns(&mut self, pos: ChunkPos) {
        self.columns
            .rebuild(pos.0.x, pos.0.z, &self.chunks, &self.materials, self.bounds);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::CHUNK_SIZE;
    use crate::edit::{CommandId, PlayerId};
    use crate::shape::Shape;
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

    /// A `Full` delta replaces the cells of a resident chunk and leaves its
    /// layers, plants and eco revision alone (SAD-P2 §3: material deltas and
    /// layer state replicate separately).
    #[test]
    fn full_delta_keeps_layers_and_plants() {
        use crate::layers::{LayerId, N_SUBSTANCES, PlantState};
        let table = phase1();
        let stone = id(&table, "stone");
        let pos = ChunkPos::new(0, 0, 0);
        let at = LocalPos::new(4, 5, 6);
        let mut chunk = Chunk::uniform(MaterialId::AIR);
        chunk.layers.set_i16(LayerId::Temperature, at, 150);
        let plant = PlantState {
            species: 2,
            stress: 0,
            age_seasons: 1,
            held: [0; N_SUBSTANCES],
        };
        chunk.plants.insert(at.index(), plant);
        chunk.eco_revision = 7;
        let mut w = World::new(table, small_bounds());
        w.insert_chunk(pos, chunk).unwrap();

        w.apply_delta(&ChunkDelta::Full {
            pos,
            revision: 1,
            storage: ChunkStorage::Uniform(stone),
        })
        .unwrap();
        let c = w.chunk(pos).unwrap();
        assert_eq!(c.storage, ChunkStorage::Uniform(stone));
        assert_eq!(c.layers.get_i16(LayerId::Temperature, at), 150);
        assert_eq!(c.plants.get(&at.index()), Some(&plant));
        assert_eq!((c.revision, c.eco_revision, c.modified), (1, 7, true));
        assert_eq!(w.surface_y(4, 6), 31);
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

    fn edit(seq: u32, op: EditOp, shape: Shape) -> EditCommand {
        let issuer = PlayerId(3);
        EditCommand {
            id: CommandId { issuer, seq },
            issuer,
            op,
            shape,
        }
    }

    fn boxed(min: (i32, i32, i32), max: (i32, i32, i32)) -> Shape {
        Shape::Box {
            min: CellPos::new(min.0, min.1, min.2),
            max: CellPos::new(max.0, max.1, max.2),
        }
    }

    fn cell_in(pos: ChunkPos, x: u32, y: u32, z: u32) -> CellPos {
        CellPos::from_chunk_local(pos, LocalPos::new(x, y, z))
    }

    /// Everything a refused delta must leave alone: every chunk's storage,
    /// revision and `modified` flag, the resident set and the damage map.
    fn assert_unchanged(w: &World, before: &World, what: &str) {
        let now: Vec<(ChunkPos, &Chunk)> = w.iter_chunks_sorted().collect();
        let then: Vec<(ChunkPos, &Chunk)> = before.iter_chunks_sorted().collect();
        assert_eq!(now, then, "{what}: chunks changed");
        assert_eq!(w.damage(), before.damage(), "{what}: damage changed");
        assert_eq!(w.hash(), before.hash(), "{what}: hash changed");
    }

    /// A paletted chunk with stone below y = 8, granite in one column, dirt
    /// on top: several palette entries and content on the chunk faces.
    fn layered_chunk(table: &MaterialTable, salt: u32) -> Chunk {
        let mut c = Chunk::uniform(id(table, "stone"));
        let granite = id(table, "granite");
        let dirt = id(table, "dirt");
        for y in 0..CHUNK_SIZE as u32 {
            c.storage
                .set(LocalPos::new(salt % 32, y, (salt / 3) % 32), granite);
        }
        for x in 0..CHUNK_SIZE as u32 {
            for z in 0..CHUNK_SIZE as u32 {
                c.storage.set(LocalPos::new(x, 31, z), dirt);
                if (x + z + salt).is_multiple_of(5) {
                    c.storage.set(LocalPos::new(x, 30, z), MaterialId::AIR);
                }
            }
        }
        c
    }

    /// Small-bounds world with every chunk resident: mixed paletted and
    /// uniform chunks, including air and bedrock.
    fn replica_test_world(table: &Arc<MaterialTable>) -> World {
        let mut w = World::new(table.clone(), small_bounds());
        for (i, pos) in small_bounds().chunks().enumerate() {
            let chunk = match i % 4 {
                0 => Chunk::uniform(id(table, "stone")),
                1 => layered_chunk(table, i as u32 * 7),
                2 => Chunk::uniform(MaterialId::AIR),
                _ if pos.0.y < 0 => {
                    let mut c = layered_chunk(table, i as u32);
                    for x in 0..CHUNK_SIZE as u32 {
                        for z in 0..CHUNK_SIZE as u32 {
                            c.storage.set(LocalPos::new(x, 0, z), id(table, "bedrock"));
                        }
                    }
                    c
                }
                _ => Chunk::uniform(id(table, "granite")),
            };
            w.insert_chunk(pos, chunk).unwrap();
        }
        w
    }

    #[test]
    fn apply_delta_requires_next_revision() {
        let table = phase1();
        let stone = id(&table, "stone");
        let granite = id(&table, "granite");
        let coal = id(&table, "coal_seam");
        let p = ChunkPos::new(0, 0, -1);
        let mut w = World::new(table.clone(), small_bounds());
        for pos in small_bounds().chunks() {
            let mut c = Chunk::uniform(stone);
            if pos == p {
                c.revision = 3;
            }
            w.insert_chunk(pos, c).unwrap();
        }
        let cells = |revision: u64, changes: Vec<(u16, MaterialId)>| ChunkDelta::Cells {
            pos: p,
            revision,
            changes,
        };
        let a = LocalPos::new(1, 2, 3);
        let b = LocalPos::new(31, 31, 31);
        let at = |l: LocalPos| CellPos::from_chunk_local(p, l);

        // Stale, repeated, skipped and far revisions are gaps; nothing changes.
        let before = w.clone();
        for got in [0, 1, 2, 3, 5, 6, 100, u64::MAX] {
            assert_eq!(
                w.apply_delta(&cells(got, vec![(a.index(), granite)])),
                Err(DeltaError::RevisionGap {
                    pos: p,
                    local: 3,
                    got
                }),
                "revision {got}"
            );
            assert_unchanged(&w, &before, &format!("revision {got}"));
        }

        // The next revision applies every change, in order.
        assert_eq!(
            w.apply_delta(&cells(4, vec![(a.index(), granite), (b.index(), coal)])),
            Ok(())
        );
        let ch = w.chunk(p).unwrap();
        assert_eq!((ch.revision, ch.modified), (4, true));
        assert_eq!(w.get_cell(at(a)), Some(granite));
        assert_eq!(w.get_cell(at(b)), Some(coal));
        assert_eq!(w.get_cell(at(LocalPos::new(1, 2, 4))), Some(stone));
        // Other chunks are untouched.
        for (pos, c) in w.iter_chunks_sorted().filter(|(pos, _)| *pos != p) {
            assert_eq!((c.revision, c.modified), (0, false), "{pos:?}");
            assert_eq!(c.storage, ChunkStorage::Uniform(stone), "{pos:?}");
        }

        // The same delta again is now stale.
        let before = w.clone();
        assert_eq!(
            w.apply_delta(&cells(4, vec![(a.index(), stone)])),
            Err(DeltaError::RevisionGap {
                pos: p,
                local: 4,
                got: 4
            })
        );
        assert_unchanged(&w, &before, "repeat");

        // A bad index or unknown material anywhere in the list refuses the
        // whole delta, including the valid changes before it.
        for (changes, err) in [
            (
                vec![(a.index(), stone), (32768, granite)],
                DeltaError::BadIndex {
                    pos: p,
                    index: 32768,
                },
            ),
            (
                vec![(a.index(), stone), (u16::MAX, granite)],
                DeltaError::BadIndex {
                    pos: p,
                    index: u16::MAX,
                },
            ),
            (
                vec![(a.index(), stone), (b.index(), MaterialId(999))],
                DeltaError::UnknownMaterial {
                    pos: p,
                    id: MaterialId(999),
                },
            ),
        ] {
            assert_eq!(w.apply_delta(&cells(5, changes)), Err(err.clone()));
            assert_unchanged(&w, &before, &format!("{err}"));
        }

        // A revision gap is reported before content problems.
        assert_eq!(
            w.apply_delta(&cells(9, vec![(40000, MaterialId(999))])),
            Err(DeltaError::RevisionGap {
                pos: p,
                local: 4,
                got: 9
            })
        );
        assert_unchanged(&w, &before, "gap and bad content");

        // Consecutive deltas keep applying; a later change to the same cell wins.
        assert_eq!(
            w.apply_delta(&cells(5, vec![(a.index(), coal), (a.index(), stone)])),
            Ok(())
        );
        assert_eq!(w.get_cell(at(a)), Some(stone));
        assert_eq!(w.chunk(p).unwrap().revision, 5);
        // An empty change list still takes its revision.
        assert_eq!(w.apply_delta(&cells(6, Vec::new())), Ok(()));
        assert_eq!(w.chunk(p).unwrap().revision, 6);

        // Out of bounds, and in bounds but not resident.
        let far = ChunkPos::new(5, 0, 0);
        let before = w.clone();
        assert_eq!(
            w.apply_delta(&ChunkDelta::Cells {
                pos: far,
                revision: 1,
                changes: vec![(0, granite)],
            }),
            Err(DeltaError::OutOfBounds(far))
        );
        assert_unchanged(&w, &before, "out of bounds");
        let mut sparse = World::new(table.clone(), small_bounds());
        sparse.insert_chunk(p, Chunk::uniform(stone)).unwrap();
        let missing = ChunkPos::new(1, 0, 1);
        let sparse_before = sparse.clone();
        assert_eq!(
            sparse.apply_delta(&ChunkDelta::Cells {
                pos: missing,
                revision: 1,
                changes: vec![(0, granite)],
            }),
            Err(DeltaError::NotResident(missing))
        );
        assert_unchanged(&sparse, &sparse_before, "not resident");
        assert_eq!(sparse.chunk_count(), 1);

        // A chunk at the last revision can take no further Cells delta.
        let mut top = Chunk::uniform(stone);
        top.revision = u64::MAX;
        sparse.insert_chunk(p, top).unwrap();
        let sparse_before = sparse.clone();
        for got in [0, u64::MAX] {
            assert_eq!(
                sparse.apply_delta(&cells(got, vec![(0, granite)])),
                Err(DeltaError::RevisionGap {
                    pos: p,
                    local: u64::MAX,
                    got
                })
            );
        }
        assert_unchanged(&sparse, &sparse_before, "u64::MAX");

        // Applied changes clear damage on the cells they write; other damage
        // in the chunk, and damage under refused deltas, is kept.
        let hit = |w: &mut World, seq, c: CellPos| {
            let r = w.apply(&edit(seq, EditOp::Damage(10), Shape::Cell(c)));
            assert_eq!(r.status, EditStatus::Applied);
            assert!(r.deltas.is_empty());
        };
        let d1 = LocalPos::new(4, 4, 4);
        let d2 = LocalPos::new(5, 4, 4);
        hit(&mut w, 1, at(d1));
        hit(&mut w, 2, at(d2));
        assert_eq!(w.damage().len(), 2);
        assert!(w.apply_delta(&cells(8, vec![(d1.index(), coal)])).is_err());
        assert_eq!(w.damage().len(), 2);
        assert_eq!(w.apply_delta(&cells(7, vec![(d1.index(), coal)])), Ok(()));
        assert_eq!(w.damage().get(at(d1)), 0);
        assert_eq!(w.damage().get(at(d2)), 10);
        assert_eq!(w.damage().len(), 1);
    }

    #[test]
    fn apply_full_delta_replaces() {
        let table = phase1();
        let stone = id(&table, "stone");
        let granite = id(&table, "granite");
        let coal = id(&table, "coal_seam");
        let dirt = id(&table, "dirt");
        let p = ChunkPos::new(1, -1, 0);
        let q = ChunkPos::new(0, -1, 0);
        let mut w = World::new(table.clone(), small_bounds());
        for pos in small_bounds().chunks() {
            w.insert_chunk(pos, layered_chunk(&table, 5)).unwrap();
        }
        // Give p a revision and q some damage via real edits.
        for seq in 0..2 {
            let r = w.apply(&edit(
                seq,
                EditOp::Set(coal),
                Shape::Cell(cell_in(p, seq, 0, 0)),
            ));
            assert_eq!(r.deltas.len(), 1);
        }
        assert_eq!(w.chunk(p).unwrap().revision, 2);
        let dmg = |w: &mut World, seq, c| {
            w.apply(&edit(seq, EditOp::Damage(1), Shape::Cell(c)));
        };
        dmg(&mut w, 10, cell_in(p, 3, 3, 3));
        dmg(&mut w, 11, cell_in(p, 9, 1, 0));
        dmg(&mut w, 12, cell_in(q, 3, 3, 3));
        assert_eq!(w.damage().len(), 3);

        // A non-canonical storage: an unused palette entry, cells in odd order.
        let mut s = Chunk::uniform(granite).storage;
        s.set(LocalPos::new(0, 0, 0), dirt);
        s.set(LocalPos::new(0, 0, 0), granite);
        for i in (0..32768u16).step_by(97) {
            s.set(LocalPos::from_index(i), stone);
        }
        let mut canonical = s.clone();
        canonical.compact();
        assert_ne!(s, canonical, "the test storage starts out non-canonical");

        let before_q = w.chunk(q).unwrap().clone();
        assert_eq!(
            w.apply_delta(&ChunkDelta::Full {
                pos: p,
                revision: 3,
                storage: s.clone(),
            }),
            Ok(())
        );
        let ch = w.chunk(p).unwrap();
        assert_eq!((ch.revision, ch.modified), (3, true));
        assert_eq!(ch.storage, canonical, "stored compacted");
        assert_eq!(ch.hash(), s.hash());
        for i in (0..32768u16).step_by(13) {
            let l = LocalPos::from_index(i);
            assert_eq!(
                w.get_cell(CellPos::from_chunk_local(p, l)),
                Some(s.get(l)),
                "{l:?}"
            );
        }
        // Damage in p is gone; the neighbour keeps its damage and contents.
        assert_eq!(w.damage().len(), 1);
        assert_eq!(w.damage().get(cell_in(q, 3, 3, 3)), 1);
        assert_eq!(w.chunk(q), Some(&before_q));

        // Full replaces whatever the local revision is: ahead, far ahead, behind.
        for rev in [4, 50, 7, u64::MAX] {
            let storage = ChunkStorage::Uniform(if rev % 2 == 0 { stone } else { coal });
            assert_eq!(
                w.apply_delta(&ChunkDelta::Full {
                    pos: p,
                    revision: rev,
                    storage: storage.clone(),
                }),
                Ok(())
            );
            let ch = w.chunk(p).unwrap();
            assert_eq!((ch.revision, ch.modified), (rev, true));
            assert_eq!(ch.storage, storage);
        }
        // A paletted storage holding one material comes out Uniform.
        let mut one = ChunkStorage::Uniform(dirt);
        one.set(LocalPos::new(2, 2, 2), stone);
        one.set(LocalPos::new(2, 2, 2), dirt);
        w.apply_delta(&ChunkDelta::Full {
            pos: p,
            revision: 1,
            storage: one,
        })
        .unwrap();
        assert_eq!(w.chunk(p).unwrap().storage, ChunkStorage::Uniform(dirt));

        // A Full delta makes an in-bounds chunk resident.
        let mut sparse = World::new(table.clone(), small_bounds());
        assert_eq!(
            sparse.apply_delta(&ChunkDelta::Full {
                pos: q,
                revision: 9,
                storage: s.clone(),
            }),
            Ok(())
        );
        assert_eq!(sparse.chunk_count(), 1);
        let ch = sparse.chunk(q).unwrap();
        assert_eq!((ch.revision, ch.modified), (9, true));
        assert_eq!(ch.storage, canonical);

        // Refused: out of bounds, malformed storage, unknown materials in use.
        let before = w.clone();
        let full = |pos, storage| ChunkDelta::Full {
            pos,
            revision: 2,
            storage,
        };
        let far = ChunkPos::new(0, 1, 2);
        assert_eq!(
            w.apply_delta(&full(far, ChunkStorage::Uniform(stone))),
            Err(DeltaError::OutOfBounds(far))
        );
        assert_unchanged(&w, &before, "out of bounds");

        let paletted =
            |palette: Vec<MaterialId>, bits: u8, data: Vec<u64>| ChunkStorage::Paletted {
                palette,
                bits,
                data,
            };
        // Cell 0 uses slot 3 of a 3-entry palette.
        let mut past_end = vec![0u64; 1024];
        past_end[0] = 0b11;
        for bad in [
            paletted(vec![stone, granite], 3, vec![0; 1536]),
            paletted(vec![stone, granite], 0, Vec::new()),
            paletted(vec![stone, granite], 32, vec![0; 16384]),
            paletted(vec![stone, granite], 1, vec![0; 511]),
            paletted(vec![stone, granite], 1, vec![0; 513]),
            paletted(Vec::new(), 1, vec![0; 512]),
            paletted(vec![stone, granite, coal], 1, vec![0; 512]),
            paletted(vec![stone, granite, coal], 2, past_end),
        ] {
            assert_eq!(
                w.apply_delta(&full(p, bad.clone())),
                Err(DeltaError::MalformedStorage(p)),
                "{bad:?}"
            );
            assert_unchanged(&w, &before, "malformed");
        }
        assert_eq!(
            w.apply_delta(&full(p, ChunkStorage::Uniform(MaterialId(4242)))),
            Err(DeltaError::UnknownMaterial {
                pos: p,
                id: MaterialId(4242)
            })
        );
        let mut unknown = ChunkStorage::Uniform(stone);
        unknown.set(LocalPos::new(31, 0, 31), MaterialId(777));
        assert_eq!(
            w.apply_delta(&full(p, unknown)),
            Err(DeltaError::UnknownMaterial {
                pos: p,
                id: MaterialId(777)
            })
        );
        assert_unchanged(&w, &before, "unknown material");

        // An unknown id in an unused palette slot is compacted away.
        let mut unused = ChunkStorage::Uniform(stone);
        unused.set(LocalPos::new(0, 0, 0), MaterialId(777));
        unused.set(LocalPos::new(1, 0, 0), granite);
        unused.set(LocalPos::new(0, 0, 0), stone);
        assert_eq!(w.apply_delta(&full(p, unused.clone())), Ok(()));
        let mut expect = unused;
        expect.compact();
        assert_eq!(w.chunk(p).unwrap().storage, expect);
    }

    /// A replica fed only the deltas from `apply` stays identical to the
    /// world that produced them: hash, contents, revisions and `modified`.
    #[test]
    fn apply_delta_matches_apply_command() {
        let table = phase1();
        let mut host = replica_test_world(&table);
        let mut replica = replica_test_world(&table);
        assert_eq!(host.hash(), replica.hash());
        let keys = [
            "air",
            "topsoil",
            "dirt",
            "stone",
            "granite",
            "coal_seam",
            "glowcrystal",
            "bedrock",
        ];
        let m = |k: &str| id(&table, k);

        // Scripted commands covering every op, borders, Full deltas, breaks,
        // no-op commands and rejected commands.
        let mut cmds = vec![
            edit(
                0,
                EditOp::Set(m("granite")),
                boxed((28, -4, -4), (35, 3, 3)),
            ),
            edit(
                1,
                EditOp::Remove,
                Shape::Sphere {
                    center: CellPos::new(0, 0, 0),
                    radius: 6,
                },
            ),
            edit(
                2,
                EditOp::Paint(m("coal_seam")),
                boxed((-32, -32, -32), (-1, -1, -1)),
            ),
            // Whole-chunk fills: Full deltas, one to Uniform.
            edit(3, EditOp::Set(m("dirt")), boxed((-32, 0, 0), (-1, 31, 31))),
            edit(
                4,
                EditOp::Set(m("stone")),
                boxed((0, -32, 32), (31, -1, 63)),
            ),
            edit(
                5,
                EditOp::Paint(m("granite")),
                boxed((32, 0, 32), (63, 31, 63)),
            ),
            // Set to what is already there: no delta.
            edit(6, EditOp::Set(m("dirt")), boxed((-32, 0, 0), (-1, 31, 31))),
            // Rejected: too many cells, entirely outside, unknown material.
            edit(
                7,
                EditOp::Set(m("stone")),
                boxed((-32, -32, -32), (63, 31, 63)),
            ),
            edit(8, EditOp::Remove, boxed((100, 0, 0), (110, 5, 5))),
            edit(
                9,
                EditOp::Set(MaterialId(999)),
                Shape::Cell(CellPos::new(1, 1, 1)),
            ),
            // Bedrock survives Remove and Damage.
            edit(10, EditOp::Remove, boxed((-32, -32, -32), (63, -32, 63))),
        ];
        // Damage until a sphere of mixed material breaks, crossing borders.
        for seq in 11..20 {
            cmds.push(edit(
                seq,
                EditOp::Damage(7),
                Shape::Sphere {
                    center: CellPos::new(32, -1, 31),
                    radius: 4,
                },
            ));
        }

        // Then pseudo-random commands from a fixed-seed LCG.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = |n: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % n
        };
        for seq in 20..260 {
            let mut coord = |lo: i32, hi: i32| lo - 2 + next((hi - lo + 5) as u64) as i32;
            let c = CellPos::new(coord(-32, 63), coord(-32, 31), coord(-32, 63));
            let material = m(keys[next(keys.len() as u64) as usize]);
            let op = match next(4) {
                0 => EditOp::Set(material),
                1 => EditOp::Remove,
                2 => EditOp::Damage([1, 15, 30, 60, u16::MAX][next(5) as usize]),
                _ => EditOp::Paint(material),
            };
            let shape = match next(4) {
                0 => Shape::Cell(c),
                1 => Shape::Sphere {
                    center: c,
                    radius: next(7) as u16,
                },
                2 => {
                    let ext = CellPos::new(
                        c.0.x + next(9) as i32,
                        c.0.y + next(9) as i32,
                        c.0.z + next(9) as i32,
                    );
                    Shape::Box { min: c, max: ext }
                }
                // Big slabs: above FULL_DELTA_THRESHOLD in one chunk.
                _ => Shape::Box {
                    min: c,
                    max: CellPos::new(c.0.x + 31, c.0.y + 7, c.0.z + 31),
                },
            };
            cmds.push(edit(seq, op, shape));
        }

        let (mut cells, mut fulls, mut rejected, mut breaks) = (0, 0, 0, 0);
        let mut last_full: Option<ChunkPos> = None;
        for (n, c) in cmds.iter().enumerate() {
            let r = host.apply(c);
            if r.status != EditStatus::Applied {
                rejected += 1;
            }
            if !r.yields.is_empty() {
                breaks += 1;
            }
            for d in &r.deltas {
                match d {
                    ChunkDelta::Cells { .. } => cells += 1,
                    ChunkDelta::Full { pos, .. } => {
                        fulls += 1;
                        last_full = Some(*pos);
                    }
                    ChunkDelta::Layers { .. } => panic!("Phase 1 edits change no layers"),
                }
                assert_eq!(replica.apply_delta(d), Ok(()), "command {n}: {d:?}");
            }
            // Revisions and `modified` everywhere; contents of the touched chunks.
            let host_state: Vec<(ChunkPos, u64, bool)> = host
                .iter_chunks_sorted()
                .map(|(p, ch)| (p, ch.revision, ch.modified))
                .collect();
            let replica_state: Vec<(ChunkPos, u64, bool)> = replica
                .iter_chunks_sorted()
                .map(|(p, ch)| (p, ch.revision, ch.modified))
                .collect();
            assert_eq!(host_state, replica_state, "command {n}");
            for d in &r.deltas {
                let (h, rep) = (
                    host.chunk(d.pos()).unwrap(),
                    replica.chunk(d.pos()).unwrap(),
                );
                assert_eq!(h.hash(), rep.hash(), "command {n}, chunk {:?}", d.pos());
            }
            if n % 40 == 0 {
                assert_eq!(host.hash(), replica.hash(), "command {n}");
            }
            // Replaying a command's deltas a second time is refused (Cells)
            // or harmless (Full), so a duplicated message cannot corrupt.
            if n % 17 == 0 {
                for d in &r.deltas {
                    if let ChunkDelta::Cells { pos, revision, .. } = d {
                        assert_eq!(
                            replica.apply_delta(d),
                            Err(DeltaError::RevisionGap {
                                pos: *pos,
                                local: *revision,
                                got: *revision
                            })
                        );
                    }
                }
            }
        }
        assert_eq!(host.hash(), replica.hash());
        assert!(replica.damage().is_empty(), "replicas never hold damage");
        if let Some(p) = last_full {
            // Both sides compact the same way, so storage matches exactly.
            assert_eq!(
                host.chunk(p).unwrap().storage,
                replica.chunk(p).unwrap().storage
            );
        }
        // The script must exercise every path it claims to.
        assert!(cells >= 50, "{cells} Cells deltas");
        assert!(fulls >= 5, "{fulls} Full deltas");
        assert!(rejected >= 3, "{rejected} rejected");
        assert!(breaks >= 1, "{breaks} commands broke cells");
        eprintln!("{cells} Cells, {fulls} Full, {rejected} rejected, {breaks} breaking");
    }

    #[test]
    fn apply_keeping_clear_skips_set_cells_only() {
        let table = phase1();
        let stone = id(&table, "stone");
        let granite = id(&table, "granite");
        let clay = id(&table, "clay");
        let mut w = World::new(table.clone(), small_bounds());
        for pos in small_bounds().chunks() {
            w.insert_chunk(pos, Chunk::uniform(stone)).unwrap();
        }
        // Two boxes, one across the chunk border at x = 0.
        let keep = [
            CellAabb::new(CellPos::new(-1, 0, 0), CellPos::new(0, 3, 1)),
            CellAabb::new(CellPos::new(10, 5, 10), CellPos::new(10, 5, 10)),
        ];
        let inside = |c: CellPos| keep.iter().any(|b| b.contains(c));

        // No boxes is plain apply.
        let mut plain = w.clone();
        let cmd = edit(0, EditOp::Set(granite), boxed((-3, -1, -1), (2, 4, 2)));
        assert_eq!(w.clone().apply_keeping_clear(&cmd, &[]), plain.apply(&cmd));

        // Damage below the hardness on a cell inside a box.
        let hurt = CellPos::new(0, 0, 0);
        assert!(table.get(stone).unwrap().hardness > 1);
        w.apply_keeping_clear(&edit(1, EditOp::Damage(1), Shape::Cell(hurt)), &keep);
        assert_eq!(w.damage().get(hurt), 1);

        // Set skips the box cells: they keep their material and damage.
        let result = w.apply_keeping_clear(&cmd, &keep);
        assert_eq!(result.status, EditStatus::Applied);
        let mut set = 0;
        for c in cmd.shape.cells() {
            let want = if inside(c) { stone } else { granite };
            assert_eq!(w.get_cell(c), Some(want), "{c:?}");
            set += usize::from(!inside(c));
        }
        assert_eq!(set, 6 * 6 * 4 - 16);
        let changed: usize = result
            .deltas
            .iter()
            .map(|d| match d {
                ChunkDelta::Cells { changes, .. } => changes.len(),
                ChunkDelta::Full { .. } => unreachable!("small edit"),
                ChunkDelta::Layers { .. } => unreachable!("Phase 1 edits change no layers"),
            })
            .sum();
        assert_eq!(changed, set);
        assert_eq!(w.damage().get(hurt), 1, "a skipped cell keeps its damage");

        // A Set wholly inside a box is Applied and changes nothing.
        let before = w.clone();
        let result = w.apply_keeping_clear(
            &edit(
                2,
                EditOp::Set(granite),
                Shape::Cell(CellPos::new(10, 5, 10)),
            ),
            &keep,
        );
        assert_eq!(result.status, EditStatus::Applied);
        assert!(result.deltas.is_empty());
        assert_unchanged(&w, &before, "Set inside a box");

        // Paint, Remove and Damage ignore the boxes.
        w.apply_keeping_clear(
            &edit(3, EditOp::Paint(clay), boxed((-1, 0, 0), (0, 1, 1))),
            &keep,
        );
        assert!(
            boxed((-1, 0, 0), (0, 1, 1))
                .cells()
                .all(|c| w.get_cell(c) == Some(clay))
        );
        w.apply_keeping_clear(
            &edit(4, EditOp::Remove, boxed((-1, 2, 0), (0, 2, 1))),
            &keep,
        );
        assert!(
            boxed((-1, 2, 0), (0, 2, 1))
                .cells()
                .all(|c| w.get_cell(c) == Some(MaterialId::AIR))
        );
        let far = CellPos::new(-1, 3, 1);
        w.apply_keeping_clear(&edit(5, EditOp::Damage(u16::MAX), Shape::Cell(far)), &keep);
        assert_eq!(w.get_cell(far), Some(MaterialId::AIR));
    }
}
