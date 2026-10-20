//! Edit commands, their results and chunk deltas (SAD §8.1, §8.3).
//!
//! [`World::apply`](crate::world::World::apply) is the only way cells change
//! on the host. It returns one [`ChunkDelta`] per chunk it changed, sorted by
//! [`ChunkPos`], so a replica can reproduce the edit exactly.

use serde::{Deserialize, Serialize};

use crate::chunk::ChunkStorage;
use crate::coords::ChunkPos;
use crate::displace::layer_value_fits;
use crate::layers::{LayerId, PlantState};
use crate::ledger::SeasonLedger;
use crate::materials::{MaterialDef, MaterialId, MaterialTable};
use crate::shape::Shape;

/// A chunk whose change count exceeds this gets a [`ChunkDelta::Full`].
pub const FULL_DELTA_THRESHOLD: usize = 4096;

/// A command whose shape has more cells than this is rejected whole (SAD §8.2).
pub const MAX_EDIT_CELLS: u64 = 32_768;

/// A connected player. The host assigns it in `Welcome`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PlayerId(pub u32);

/// Identifies a command: its issuer and the issuer's sequence number.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct CommandId {
    pub issuer: PlayerId,
    pub seq: u32,
}

/// What an edit does to each cell of its shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EditOp {
    /// Place / fill: every cell becomes this material, overwriting what is there.
    Set(MaterialId),
    /// Every diggable cell becomes air. No yield (editor tool).
    Remove,
    /// Pickaxe: accumulate damage; a broken cell becomes air and yields.
    Damage(u16),
    /// Every `Solid` cell becomes this material; other cells are untouched.
    Paint(MaterialId),
    /// Every cell's `layer` becomes `value` (SAD-P2 §9). Materials do not
    /// change. Gas layers are written on air cells only.
    SetLayer { layer: LayerId, value: i32 },
}

impl EditOp {
    /// Checks the op against the material table before any cell is touched.
    pub(crate) fn validate(self, materials: &MaterialTable) -> Result<(), RejectReason> {
        match self {
            EditOp::Set(id) | EditOp::Paint(id) if materials.get(id).is_none() => {
                Err(RejectReason::UnknownMaterial(id))
            }
            EditOp::SetLayer { layer, value }
                if !layer.is_valid() || !layer_value_fits(layer, value) =>
            {
                Err(RejectReason::InvalidLayerValue { layer, value })
            }
            EditOp::Set(_)
            | EditOp::Paint(_)
            | EditOp::Remove
            | EditOp::Damage(_)
            | EditOp::SetLayer { .. } => Ok(()),
        }
    }

    /// The material a cell holding `old` becomes, or `None` if the op skips it.
    /// Only meaningful for ops that passed [`EditOp::validate`]. `Damage`
    /// returns `None`: whether it breaks a cell depends on the cell's
    /// accumulated damage, which [`World::apply`](crate::world::World::apply)
    /// tracks. `SetLayer` changes no material and returns `None`.
    pub(crate) fn target(self, old: Option<&MaterialDef>) -> Option<MaterialId> {
        match self {
            EditOp::Set(id) => Some(id),
            EditOp::Remove => old.is_some_and(|d| d.diggable).then_some(MaterialId::AIR),
            EditOp::Paint(id) => old.is_some_and(|d| d.is_solid()).then_some(id),
            EditOp::Damage(_) | EditOp::SetLayer { .. } => None,
        }
    }
}

/// An edit request: the client sends it, the host validates and applies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EditCommand {
    pub id: CommandId,
    pub issuer: PlayerId,
    pub op: EditOp,
    pub shape: Shape,
}

/// Why a whole command was rejected. A rejected command changes nothing.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum RejectReason {
    #[error("material {0:?} is not in the material table")]
    UnknownMaterial(MaterialId),
    #[error("shape has more than {MAX_EDIT_CELLS} cells")]
    TooManyCells,
    #[error("shape lies entirely outside the world bounds")]
    OutsideBounds,
    #[error("value {value} does not fit layer {layer:?}")]
    InvalidLayerValue { layer: LayerId, value: i32 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditStatus {
    Applied,
    Rejected(RejectReason),
}

/// What changed in one chunk. `revision` is the chunk's revision after the change.
///
/// Like [`ChunkStorage`], a delta decoded from outside the process is
/// untrusted; [`World::apply_delta`](crate::world::World::apply_delta) checks it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChunkDelta {
    /// Changed cells as (local linear index, new material), ascending by index.
    Cells {
        pos: ChunkPos,
        revision: u64,
        changes: Vec<(u16, MaterialId)>,
    },
    /// The whole compacted chunk, sent when more than
    /// [`FULL_DELTA_THRESHOLD`] cells changed.
    Full {
        pos: ChunkPos,
        revision: u64,
        storage: ChunkStorage,
    },
    /// Layer and plant changes from `SetLayer` and from the displacement
    /// rules of material edits (SAD-P2 §6, §7.0). `eco_revision` is the
    /// chunk's eco revision after the change. `changes` are (local index,
    /// layer, new value) ascending by (index, layer); `plants` are (local
    /// index, new state or `None` when removed) ascending by index.
    Layers {
        pos: ChunkPos,
        eco_revision: u64,
        changes: Vec<(u16, LayerId, i32)>,
        plants: Vec<(u16, Option<PlantState>)>,
    },
}

impl ChunkDelta {
    pub fn pos(&self) -> ChunkPos {
        match self {
            ChunkDelta::Cells { pos, .. }
            | ChunkDelta::Full { pos, .. }
            | ChunkDelta::Layers { pos, .. } => *pos,
        }
    }

    /// The revision the delta brings its chunk to: `revision` for `Cells`
    /// and `Full`, `eco_revision` for `Layers`.
    pub fn revision(&self) -> u64 {
        match self {
            ChunkDelta::Cells { revision, .. } | ChunkDelta::Full { revision, .. } => *revision,
            ChunkDelta::Layers { eco_revision, .. } => *eco_revision,
        }
    }
}

/// The outcome of [`World::apply`](crate::world::World::apply).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditResult {
    pub command: CommandId,
    pub status: EditStatus,
    /// Ascending by [`ChunkPos`]: per changed chunk its material delta
    /// (`Cells` or `Full`) if cells changed, then its `Layers` delta if
    /// layers or plants changed. Empty when rejected or when nothing changed.
    pub deltas: Vec<ChunkDelta>,
    /// Materials produced by broken cells and harvested plants, as
    /// (material, count).
    pub yields: Vec<(MaterialId, u32)>,
    /// The edit's displacement and `SetLayer` terms (SAD-P2 §7.0), which the
    /// host merges into its between-seasons ledger.
    /// Boxed: a ledger is large and most results carry an empty one.
    pub ledger: Box<SeasonLedger>,
}

impl EditResult {
    pub(crate) fn rejected(command: CommandId, reason: RejectReason) -> Self {
        Self {
            command,
            status: EditStatus::Rejected(reason),
            deltas: Vec::new(),
            yields: Vec::new(),
            ledger: Box::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::chunk::Chunk;
    use crate::coords::{CellAabb, CellPos, LocalPos};
    use crate::world::World;
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

    /// A 2 × 1 × 1 chunk world (cells 0..=63 × 0..=31 × 0..=31), every chunk
    /// resident and filled with `fill`.
    fn two_chunk_world(fill: &str) -> World {
        let table = phase1();
        let fill = id(&table, fill);
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 31));
        let mut w = World::new(table, bounds);
        for pos in bounds.chunks() {
            w.insert_chunk(pos, Chunk::uniform(fill)).unwrap();
        }
        w
    }

    fn cmd(seq: u32, op: EditOp, shape: Shape) -> EditCommand {
        let issuer = PlayerId(7);
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

    fn cells_delta(d: &ChunkDelta) -> (ChunkPos, u64, &[(u16, MaterialId)]) {
        match d {
            ChunkDelta::Cells {
                pos,
                revision,
                changes,
            } => (*pos, *revision, changes),
            other => panic!("expected Cells delta for {:?}, got {other:?}", other.pos()),
        }
    }

    /// Replays deltas onto a copy of `before` through `World::apply_delta`,
    /// checking that the deltas carry the whole edit.
    fn replay(before: &World, deltas: &[ChunkDelta]) -> World {
        let mut w = before.clone();
        replay_in_place(&mut w, deltas);
        w
    }

    /// Like a replica: every delta names a resident chunk at the next
    /// revision (`Full` included, though `apply_delta` does not require it).
    fn replay_in_place(w: &mut World, deltas: &[ChunkDelta]) {
        for d in deltas {
            let pos = d.pos();
            let chunk = w.chunk(pos).expect("delta for a resident chunk");
            assert_eq!(d.revision(), chunk.revision + 1, "revision for {pos:?}");
            w.apply_delta(d)
                .unwrap_or_else(|e| panic!("delta for {pos:?}: {e}"));
        }
    }

    #[test]
    fn set_produces_cells_delta() {
        let mut w = two_chunk_world("stone");
        let table = w.materials().clone();
        let stone = id(&table, "stone");
        let granite = id(&table, "granite");
        let before = w.clone();

        // A 4 × 2 × 2 box straddling the x = 31 | 32 chunk border.
        let c = cmd(1, EditOp::Set(granite), boxed((30, 5, 9), (33, 6, 10)));
        let r = w.apply(&c);
        assert_eq!(r.command, c.id);
        assert_eq!(r.status, EditStatus::Applied);
        assert!(r.yields.is_empty());
        assert_eq!(r.deltas.len(), 2);

        let (p0, rev0, ch0) = cells_delta(&r.deltas[0]);
        let (p1, rev1, ch1) = cells_delta(&r.deltas[1]);
        assert_eq!((p0, rev0), (ChunkPos::new(0, 0, 0), 1));
        assert_eq!((p1, rev1), (ChunkPos::new(1, 0, 0), 1));
        let expect = |xs: [u32; 2]| -> Vec<(u16, MaterialId)> {
            let mut v = Vec::new();
            for y in 5..=6 {
                for z in 9..=10 {
                    for x in xs {
                        v.push((LocalPos::new(x, y, z).index(), granite));
                    }
                }
            }
            v
        };
        assert_eq!(ch0, expect([30, 31]).as_slice());
        assert_eq!(ch1, expect([0, 1]).as_slice());
        // Ascending by local index.
        for ch in [ch0, ch1] {
            assert!(ch.windows(2).all(|p| p[0].0 < p[1].0));
        }

        // The world changed exactly there.
        for cell in c.shape.cells() {
            assert_eq!(w.get_cell(cell), Some(granite), "{cell:?}");
        }
        assert_eq!(w.get_cell(CellPos::new(29, 5, 9)), Some(stone));
        assert_eq!(w.get_cell(CellPos::new(34, 6, 10)), Some(stone));
        for pos in [p0, p1] {
            let chunk = w.chunk(pos).unwrap();
            assert_eq!(chunk.revision, 1);
            assert!(chunk.modified);
        }
        // Replaying the deltas reproduces the edit.
        assert_eq!(replay(&before, &r.deltas).hash(), w.hash());

        // Overwriting with a different material: only changed cells are
        // listed; cells already holding the material are skipped, and the
        // untouched chunk keeps its revision.
        let basalt = id(&table, "basalt");
        let before = w.clone();
        let r = w.apply(&cmd(2, EditOp::Set(basalt), boxed((31, 5, 9), (31, 7, 9))));
        assert_eq!(r.status, EditStatus::Applied);
        assert_eq!(r.deltas.len(), 1);
        let (p, rev, ch) = cells_delta(&r.deltas[0]);
        assert_eq!((p, rev), (ChunkPos::new(0, 0, 0), 2));
        assert_eq!(ch.len(), 3);
        assert_eq!(w.chunk(ChunkPos::new(1, 0, 0)).unwrap().revision, 1);
        assert_eq!(replay(&before, &r.deltas).hash(), w.hash());

        let r = w.apply(&cmd(3, EditOp::Set(basalt), boxed((31, 5, 9), (31, 7, 9))));
        assert_eq!(r.status, EditStatus::Applied);
        assert!(r.deltas.is_empty(), "no cell changed: {:?}", r.deltas);
        assert_eq!(w.chunk(ChunkPos::new(0, 0, 0)).unwrap().revision, 2);

        // Cells outside the bounds or in a non-resident chunk are skipped.
        let mut sparse = World::new(table.clone(), CellAabb::PHASE1_DEFAULT);
        sparse
            .insert_chunk(ChunkPos::new(0, 0, 0), Chunk::uniform(stone))
            .unwrap();
        let r = sparse.apply(&cmd(4, EditOp::Set(granite), boxed((-2, 0, 0), (33, 0, 0))));
        assert_eq!(r.status, EditStatus::Applied);
        assert_eq!(r.deltas.len(), 1);
        let (p, rev, ch) = cells_delta(&r.deltas[0]);
        assert_eq!((p, rev, ch.len()), (ChunkPos::new(0, 0, 0), 1, 32));
        assert_eq!(sparse.chunk_count(), 1);

        // An unknown material rejects the whole command and changes nothing.
        let before = w.hash();
        let bogus = MaterialId(table.len() as u16);
        let r = w.apply(&cmd(5, EditOp::Set(bogus), boxed((0, 0, 0), (3, 3, 3))));
        assert_eq!(
            r.status,
            EditStatus::Rejected(RejectReason::UnknownMaterial(bogus))
        );
        assert!(r.deltas.is_empty() && r.yields.is_empty());
        assert_eq!(w.hash(), before);
        assert_eq!(w.chunk(ChunkPos::new(0, 0, 0)).unwrap().revision, 2);
    }

    #[test]
    fn paint_skips_air() {
        let mut w = two_chunk_world("air");
        let table = w.materials().clone();
        let air = MaterialId::AIR;
        let stone = id(&table, "stone");
        let bedrock = id(&table, "bedrock");
        let copper = id(&table, "copper_ore");

        // A checkerboard floor of stone at y = 0 and a bedrock cell at y = 1.
        w.apply(&cmd(1, EditOp::Set(stone), boxed((0, 0, 0), (3, 0, 0))));
        w.apply(&cmd(2, EditOp::Set(air), boxed((1, 0, 0), (1, 0, 0))));
        w.apply(&cmd(3, EditOp::Set(air), boxed((3, 0, 0), (3, 0, 0))));
        w.apply(&cmd(4, EditOp::Set(bedrock), boxed((2, 1, 0), (2, 1, 0))));
        let rev = w.chunk(ChunkPos::new(0, 0, 0)).unwrap().revision;
        let before = w.clone();

        let r = w.apply(&cmd(5, EditOp::Paint(copper), boxed((0, 0, 0), (3, 1, 0))));
        assert_eq!(r.status, EditStatus::Applied);
        assert!(r.yields.is_empty());
        assert_eq!(r.deltas.len(), 1);
        let (p, revision, ch) = cells_delta(&r.deltas[0]);
        assert_eq!((p, revision), (ChunkPos::new(0, 0, 0), rev + 1));
        // Only the Solid cells, in (y, z, x) order. Paint does not check
        // `diggable`, so bedrock is painted too (SAD §8.2 limits that check to
        // Damage and Remove).
        assert_eq!(
            ch,
            &[
                (LocalPos::new(0, 0, 0).index(), copper),
                (LocalPos::new(2, 0, 0).index(), copper),
                (LocalPos::new(2, 1, 0).index(), copper),
            ]
        );
        for (cell, m) in [
            ((0, 0, 0), copper),
            ((1, 0, 0), air),
            ((2, 0, 0), copper),
            ((3, 0, 0), air),
            ((0, 1, 0), air),
            ((2, 1, 0), copper),
        ] {
            assert_eq!(w.get_cell(CellPos::new(cell.0, cell.1, cell.2)), Some(m));
        }
        assert_eq!(replay(&before, &r.deltas).hash(), w.hash());

        // Painting a region of only air changes nothing.
        let r = w.apply(&cmd(
            6,
            EditOp::Paint(copper),
            boxed((40, 10, 10), (50, 20, 20)),
        ));
        assert_eq!(r.status, EditStatus::Applied);
        assert!(r.deltas.is_empty());
        // Painting with the material already there changes nothing.
        let r = w.apply(&cmd(7, EditOp::Paint(copper), boxed((0, 0, 0), (3, 1, 0))));
        assert!(r.deltas.is_empty());
        assert_eq!(w.chunk(ChunkPos::new(0, 0, 0)).unwrap().revision, rev + 1);

        // An unknown paint material is rejected.
        let bogus = MaterialId(u16::MAX);
        let before = w.hash();
        let r = w.apply(&cmd(8, EditOp::Paint(bogus), boxed((0, 0, 0), (3, 1, 0))));
        assert_eq!(
            r.status,
            EditStatus::Rejected(RejectReason::UnknownMaterial(bogus))
        );
        assert_eq!(w.hash(), before);
    }

    #[test]
    fn remove_skips_undiggable() {
        let mut w = two_chunk_world("stone");
        let table = w.materials().clone();
        let air = MaterialId::AIR;
        let stone = id(&table, "stone");
        let bedrock = id(&table, "bedrock");
        assert!(!table.get(bedrock).unwrap().diggable);

        // A bedrock floor at y = 0 under stone, and a pocket of air.
        w.apply(&cmd(1, EditOp::Set(bedrock), boxed((0, 0, 0), (63, 0, 31))));
        w.apply(&cmd(2, EditOp::Remove, boxed((33, 1, 0), (33, 1, 0))));
        let before = w.clone();
        let revs =
            [ChunkPos::new(0, 0, 0), ChunkPos::new(1, 0, 0)].map(|p| w.chunk(p).unwrap().revision);

        // A column through the bedrock floor and stone above, both chunks.
        let r = w.apply(&cmd(3, EditOp::Remove, boxed((31, 0, 0), (33, 2, 0))));
        assert_eq!(r.status, EditStatus::Applied);
        assert!(r.yields.is_empty(), "Remove yields nothing");
        assert_eq!(r.deltas.len(), 2);
        let (p0, rev0, ch0) = cells_delta(&r.deltas[0]);
        let (p1, rev1, ch1) = cells_delta(&r.deltas[1]);
        assert_eq!((p0, rev0), (ChunkPos::new(0, 0, 0), revs[0] + 1));
        assert_eq!((p1, rev1), (ChunkPos::new(1, 0, 0), revs[1] + 1));
        // Chunk 0: x = 31 at y = 1, 2. Chunk 1: x = 0, 1 at y = 1, 2 except
        // the already-air cell (local 1, 1, 0).
        assert_eq!(
            ch0,
            &[
                (LocalPos::new(31, 1, 0).index(), air),
                (LocalPos::new(31, 2, 0).index(), air),
            ]
        );
        assert_eq!(
            ch1,
            &[
                (LocalPos::new(0, 1, 0).index(), air),
                (LocalPos::new(0, 2, 0).index(), air),
                (LocalPos::new(1, 2, 0).index(), air),
            ]
        );
        for x in 31..=33 {
            assert_eq!(w.get_cell(CellPos::new(x, 0, 0)), Some(bedrock), "x {x}");
            assert_eq!(w.get_cell(CellPos::new(x, 1, 0)), Some(air));
            assert_eq!(w.get_cell(CellPos::new(x, 2, 0)), Some(air));
            assert_eq!(w.get_cell(CellPos::new(x, 3, 0)), Some(stone));
        }
        assert_eq!(replay(&before, &r.deltas).hash(), w.hash());

        // Removing only bedrock and air changes nothing.
        let r = w.apply(&cmd(4, EditOp::Remove, boxed((0, 0, 0), (63, 1, 0))));
        let changed: usize = r.deltas.iter().map(|d| cells_delta(d).2.len()).sum();
        // Row y = 1 still has stone except x = 31..=33.
        assert_eq!(changed, 64 - 3);
        let r = w.apply(&cmd(5, EditOp::Remove, boxed((0, 0, 0), (63, 1, 0))));
        assert_eq!(r.status, EditStatus::Applied);
        assert!(r.deltas.is_empty());
        for x in 0..64 {
            assert_eq!(w.get_cell(CellPos::new(x, 0, 0)), Some(bedrock));
        }
    }

    #[test]
    fn full_delta_over_threshold() {
        let table = phase1();
        let stone = id(&table, "stone");
        let granite = id(&table, "granite");
        let region = boxed((0, 0, 0), (31, 4, 31)); // 32 × 5 × 32 = 5120 cells
        assert_eq!(region.cell_count(), 5120);

        // Pre-set `already` cells of the region to granite, so the command
        // changes exactly 5120 - already cells. The threshold counts changed
        // cells, not shape cells.
        let run = |already: u16| -> (World, World, EditResult) {
            let mut w = two_chunk_world("stone");
            let mut chunk = w.chunk(ChunkPos::new(0, 0, 0)).unwrap().clone();
            for i in 0..already {
                chunk.storage.set(LocalPos::from_index(i), granite);
            }
            w.insert_chunk(ChunkPos::new(0, 0, 0), chunk).unwrap();
            let before = w.clone();
            let r = w.apply(&cmd(1, EditOp::Set(granite), region));
            (before, w, r)
        };

        // 4096 changes: still Cells.
        let (before, w, r) = run(1024);
        assert_eq!(r.status, EditStatus::Applied);
        assert_eq!(r.deltas.len(), 1);
        let (_, rev, ch) = cells_delta(&r.deltas[0]);
        assert_eq!((rev, ch.len()), (1, FULL_DELTA_THRESHOLD));
        assert_eq!(replay(&before, &r.deltas).hash(), w.hash());

        // 4097 changes: Full, carrying the compacted chunk.
        let (before, w, r) = run(1023);
        assert_eq!(r.deltas.len(), 1);
        let ChunkDelta::Full {
            pos,
            revision,
            storage,
        } = &r.deltas[0]
        else {
            panic!("expected Full, got {:?}", r.deltas[0]);
        };
        assert_eq!((*pos, *revision), (ChunkPos::new(0, 0, 0), 1));
        let chunk = w.chunk(*pos).unwrap();
        assert_eq!(
            &chunk.storage, storage,
            "world holds the same compacted storage"
        );
        assert_eq!(chunk.revision, 1);
        assert!(chunk.modified);
        let mut recompacted = storage.clone();
        recompacted.compact();
        assert_eq!(&recompacted, storage, "Full storage is compacted");
        assert_eq!(storage.hash(), chunk.hash());
        for i in 0..32768u16 {
            let expect = if i < 5120 { granite } else { stone };
            assert_eq!(storage.get(LocalPos::from_index(i)), expect, "index {i}");
        }
        assert_eq!(replay(&before, &r.deltas).hash(), w.hash());

        // A whole-chunk fill compacts to Uniform; the neighbour chunk, with
        // only a few changes in the same command, gets Cells. The second
        // command skips the z = 0 slab the first already set, so it stays
        // within MAX_EDIT_CELLS (33 × 32 × 31 = 32,736 cells).
        let mut w = two_chunk_world("stone");
        let before = w.clone();
        let r = w.apply(&cmd(2, EditOp::Set(granite), boxed((0, 0, 0), (32, 31, 0))));
        assert_eq!(r.deltas.len(), 2);
        // Chunk 0 changes 32 × 32 = 1024 cells: Cells.
        assert_eq!(cells_delta(&r.deltas[0]).2.len(), 1024);
        let r2 = w.apply(&cmd(
            3,
            EditOp::Set(granite),
            boxed((0, 0, 1), (32, 31, 31)),
        ));
        assert_eq!(r2.deltas.len(), 2);
        match &r2.deltas[0] {
            ChunkDelta::Full {
                pos,
                revision,
                storage,
            } => {
                assert_eq!((*pos, *revision), (ChunkPos::new(0, 0, 0), 2));
                assert_eq!(storage, &ChunkStorage::Uniform(granite));
            }
            d => panic!("expected Full, got {d:?}"),
        }
        let (p, rev, ch) = cells_delta(&r2.deltas[1]);
        assert_eq!(
            (p, rev, ch.len()),
            (ChunkPos::new(1, 0, 0), 2, 32 * 32 - 32)
        );
        let replayed = replay(&replay(&before, &r.deltas), &r2.deltas);
        assert_eq!(replayed.hash(), w.hash());
    }

    /// Applies `c` and checks it is rejected with `reason` and that the world
    /// is untouched: same hash, revisions and `modified` flags.
    fn assert_rejected(w: &mut World, c: &EditCommand, reason: RejectReason) {
        let before = w.clone();
        let r = w.apply(c);
        assert_eq!(r.command, c.id);
        assert_eq!(r.status, EditStatus::Rejected(reason), "{c:?}");
        assert!(r.deltas.is_empty(), "rejected command has deltas");
        assert!(r.yields.is_empty(), "rejected command has yields");
        assert_eq!(w.hash(), before.hash(), "rejected command changed cells");
        let flags = |w: &World| -> Vec<(ChunkPos, u64, bool)> {
            w.iter_chunks_sorted()
                .map(|(p, c)| (p, c.revision, c.modified))
                .collect()
        };
        assert_eq!(flags(w), flags(&before));
    }

    fn sphere(center: (i32, i32, i32), radius: u16) -> Shape {
        Shape::Sphere {
            center: CellPos::new(center.0, center.1, center.2),
            radius,
        }
    }

    /// Number of cells a command changed, over all its deltas.
    fn changed_cells(r: &EditResult) -> usize {
        r.deltas.iter().map(|d| cells_delta(d).2.len()).sum()
    }

    #[test]
    fn oversized_rejected_no_change() {
        let mut w = two_chunk_world("stone");
        let table = w.materials().clone();
        let granite = id(&table, "granite");
        // Edit something first, so revisions and `modified` are non-trivial.
        w.apply(&cmd(1, EditOp::Set(granite), boxed((30, 0, 0), (33, 0, 0))));

        // Exactly MAX_EDIT_CELLS is allowed; one more is not. Both rows are
        // mostly outside the 64-cell-wide world, so only size decides.
        let row = |len: i32| boxed((0, 1, 0), (len - 1, 1, 0));
        assert_eq!(row(32_769).cell_count(), MAX_EDIT_CELLS + 1);
        for op in [EditOp::Set(granite), EditOp::Remove, EditOp::Paint(granite)] {
            assert_rejected(&mut w, &cmd(2, op, row(32_769)), RejectReason::TooManyCells);
        }
        assert_eq!(row(32_768).cell_count(), MAX_EDIT_CELLS);
        let r = w.apply(&cmd(3, EditOp::Set(granite), row(32_768)));
        assert_eq!(r.status, EditStatus::Applied);
        assert_eq!(changed_cells(&r), 64, "only the in-bounds cells change");

        // Spheres: r = 19 has 28,671 cells, r = 20 has 33,401 (OEIS A000605).
        assert_eq!(sphere((31, 15, 15), 19).cell_count(), 28_671);
        assert_eq!(sphere((31, 15, 15), 20).cell_count(), 33_401);
        let r = w.apply(&cmd(4, EditOp::Remove, sphere((31, 15, 15), 19)));
        assert_eq!(r.status, EditStatus::Applied);
        assert!(!r.deltas.is_empty());
        assert_rejected(
            &mut w,
            &cmd(5, EditOp::Set(granite), sphere((31, 15, 15), 20)),
            RejectReason::TooManyCells,
        );

        // An oversized box entirely inside the bounds is rejected whole, not
        // applied up to the limit: the world is 65,536 cells.
        assert_rejected(
            &mut w,
            &cmd(6, EditOp::Set(granite), boxed((0, 0, 0), (63, 31, 31))),
            RejectReason::TooManyCells,
        );

        // Hostile sizes are rejected quickly, without iterating or summing
        // every row: the largest radius, and a box spanning all of i32.
        let start = std::time::Instant::now();
        assert_rejected(
            &mut w,
            &cmd(7, EditOp::Remove, sphere((31, 15, 15), u16::MAX)),
            RejectReason::TooManyCells,
        );
        assert_rejected(
            &mut w,
            &cmd(
                8,
                EditOp::Remove,
                boxed(
                    (i32::MIN, i32::MIN, i32::MIN),
                    (i32::MAX, i32::MAX, i32::MAX),
                ),
            ),
            RejectReason::TooManyCells,
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "hostile shapes took {:?}",
            start.elapsed()
        );

        // Size is checked before bounds: an oversized shape entirely outside
        // reports TooManyCells.
        assert_rejected(
            &mut w,
            &cmd(9, EditOp::Remove, boxed((100, 0, 0), (100, 0, 40_000))),
            RejectReason::TooManyCells,
        );
    }

    #[test]
    fn entirely_outside_rejected() {
        // Cells 0..=63 × 0..=31 × 0..=31.
        let mut w = two_chunk_world("stone");
        let table = w.materials().clone();
        let granite = id(&table, "granite");
        let ops = [EditOp::Set(granite), EditOp::Remove, EditOp::Paint(granite)];

        let outside = [
            // Just past each face.
            Shape::Cell(CellPos::new(-1, 0, 0)),
            Shape::Cell(CellPos::new(0, -1, 0)),
            Shape::Cell(CellPos::new(0, 0, -1)),
            Shape::Cell(CellPos::new(64, 31, 31)),
            Shape::Cell(CellPos::new(63, 32, 31)),
            Shape::Cell(CellPos::new(63, 31, 32)),
            // Boxes beside the world, overlapping it on two axes.
            boxed((64, 0, 0), (70, 31, 31)),
            boxed((-10, -5, 0), (63, -1, 31)),
            boxed((0, 0, 32), (63, 31, 40)),
            // Far away, and at the i32 edges.
            boxed((1000, 1000, 1000), (1003, 1003, 1003)),
            Shape::Cell(CellPos::new(i32::MIN, i32::MIN, i32::MIN)),
            sphere((i32::MAX, i32::MAX, i32::MAX), 3),
            // A sphere whose bounding box overlaps the corner (0, 0, 0) but
            // none of whose cells do: the nearest, (0, 0, 0), is at distance²
            // 27 > 16.
            sphere((-3, -3, -3), 4),
            // An empty box (min > max) has no cells, so none inside.
            boxed((5, 5, 5), (4, 5, 5)),
        ];
        assert_eq!(
            sphere((-3, -3, -3), 4).aabb().unwrap().max,
            CellPos::new(1, 1, 1)
        );
        for shape in outside {
            for op in ops {
                assert_rejected(&mut w, &cmd(1, op, shape), RejectReason::OutsideBounds);
            }
        }

        // One cell inside is enough: the shape is not entirely outside.
        for shape in [
            Shape::Cell(CellPos::new(0, 0, 0)),
            Shape::Cell(CellPos::new(63, 31, 31)),
            sphere((-3, -3, -3), 6),
            boxed((63, 31, 31), (70, 40, 40)),
        ] {
            let r = w.apply(&cmd(2, EditOp::Set(granite), shape));
            assert_eq!(r.status, EditStatus::Applied, "{shape:?}");
        }
        assert_eq!(w.get_cell(CellPos::new(0, 0, 0)), Some(granite));
        assert_eq!(w.get_cell(CellPos::new(1, 0, 0)), Some(granite));
        assert_eq!(w.get_cell(CellPos::new(63, 31, 31)), Some(granite));

        // A shape inside the bounds over a chunk that is not resident yet is
        // not "outside": it applies and changes nothing there.
        let mut sparse = World::new(table.clone(), CellAabb::PHASE1_DEFAULT);
        sparse
            .insert_chunk(ChunkPos::new(0, 0, 0), Chunk::uniform(granite))
            .unwrap();
        let r = sparse.apply(&cmd(3, EditOp::Remove, boxed((100, 0, 0), (101, 1, 1))));
        assert_eq!(r.status, EditStatus::Applied);
        assert!(r.deltas.is_empty());
    }

    #[test]
    fn partially_outside_applies_inside() {
        // Cells 0..=63 × 0..=31 × 0..=31.
        let mut w = two_chunk_world("stone");
        let table = w.materials().clone();
        let stone = id(&table, "stone");
        let granite = id(&table, "granite");
        let bounds = w.bounds();
        let all_cells: Vec<CellPos> = boxed((0, 0, 0), (63, 31, 31)).cells().collect();

        let shapes = [
            // Over the min corner.
            boxed((-4, -4, -4), (3, 3, 3)),
            // Over the max corner, past every max face.
            boxed((60, 28, 28), (70, 40, 40)),
            // A sphere centred outside the world, poking in at the corner.
            sphere((-3, -3, -3), 6),
            // A sphere centred inside on the chunk border, cut by the y = 0
            // and z = 31 faces.
            sphere((31, 2, 30), 5),
            // A row through the whole world and beyond on both sides.
            boxed((-100, 7, 7), (200, 7, 7)),
        ];
        for (seq, shape) in (1..).zip(shapes) {
            let mut w2 = w.clone();
            let r = w2.apply(&cmd(seq, EditOp::Set(granite), shape));
            assert_eq!(r.status, EditStatus::Applied, "{shape:?}");
            assert!(r.yields.is_empty());

            let inside: Vec<CellPos> = shape.cells().filter(|&c| bounds.contains(c)).collect();
            assert!(!inside.is_empty());
            assert!(inside.len() < shape.cell_count() as usize, "{shape:?}");
            assert_eq!(changed_cells(&r), inside.len(), "{shape:?}");
            for &c in &inside {
                assert_eq!(w2.get_cell(c), Some(granite), "{c:?}");
            }
            // Every other cell of the world kept its material.
            let granite_cells = all_cells
                .iter()
                .filter(|&&c| w2.get_cell(c) == Some(granite))
                .count();
            assert_eq!(granite_cells, inside.len(), "{shape:?}");
            assert_eq!(replay(&w, &r.deltas).hash(), w2.hash());
        }

        // Applied for real, one after another, the edits accumulate. (The
        // corner sphere then changes nothing: the first box already cleared
        // its in-bounds cells.)
        for (seq, shape) in (10..).zip(shapes) {
            let r = w.apply(&cmd(seq, EditOp::Remove, shape));
            assert_eq!(r.status, EditStatus::Applied, "{shape:?}");
        }
        assert_eq!(w.get_cell(CellPos::new(0, 0, 0)), Some(MaterialId::AIR));
        assert_eq!(w.get_cell(CellPos::new(63, 31, 31)), Some(MaterialId::AIR));
        assert_eq!(w.get_cell(CellPos::new(0, 7, 7)), Some(MaterialId::AIR));
        assert_eq!(w.get_cell(CellPos::new(40, 20, 20)), Some(stone));
    }

    /// Cells -32..=40 × -32..=-1 × -32..=31: 3 × 1 × 2 chunks, negative
    /// coordinates inside, and the x = 1 chunk column cut by the max face.
    /// One chunk layer keeps the case fast; chunk hashing dominates its cost.
    fn determinism_bounds() -> CellAabb {
        CellAabb::new(CellPos::new(-32, -32, -32), CellPos::new(40, -1, 31))
    }

    const DETERMINISM_CHUNKS: usize = 6;

    /// Initial contents of one chunk: whether it is resident, its fill, and
    /// a few scattered cells, as indices into the material table.
    type ChunkSpec = (bool, u16, Vec<(u16, u16)>);

    fn chunk_spec(materials: u16) -> impl Strategy<Value = ChunkSpec> {
        (
            proptest::bool::weighted(0.9),
            0..materials,
            proptest::collection::vec((any::<u16>(), 0..materials), 0..6),
        )
    }

    fn build_chunk((_, fill, cells): &ChunkSpec) -> Chunk {
        let mut chunk = Chunk::uniform(MaterialId(*fill));
        for &(i, m) in cells {
            chunk
                .storage
                .set(LocalPos::from_index(i % 32_768), MaterialId(m));
        }
        chunk
    }

    /// The same contents in a different `ChunkStorage` structure: a stray
    /// palette entry (or Paletted instead of Uniform), so the replicas start
    /// equal by content but not by representation.
    fn restructure(mut chunk: Chunk, spare: MaterialId) -> Chunk {
        let at = LocalPos::from_index(12_345);
        let old = chunk.storage.get(at);
        let other = if old == spare { MaterialId::AIR } else { spare };
        chunk.storage.set(at, other);
        chunk.storage.set(at, old);
        chunk
    }

    fn coord(lo: i32, hi: i32) -> impl Strategy<Value = i32> {
        lo..=hi
    }

    fn cell_pos() -> impl Strategy<Value = CellPos> {
        // A margin past every face, so some shapes are partly or wholly outside.
        (coord(-36, 44), coord(-35, 2), coord(-36, 35)).prop_map(|(x, y, z)| CellPos::new(x, y, z))
    }

    fn shape_strategy() -> impl Strategy<Value = Shape> {
        prop_oneof![
            3 => cell_pos().prop_map(Shape::Cell),
            // Small boxes, including empty ones (a negative extent).
            6 => (cell_pos(), -1..12i32, -1..12i32, -1..12i32).prop_map(|(min, dx, dy, dz)| {
                Shape::Box { min, max: CellPos::new(min.0.x + dx, min.0.y + dy, min.0.z + dz) }
            }),
            5 => (cell_pos(), 0..8u16).prop_map(|(center, radius)| Shape::Sphere { center, radius }),
            // Big enough to cross FULL_DELTA_THRESHOLD in one chunk.
            1 => (coord(-36, 8), coord(-36, -4), coord(-36, 0)).prop_map(|(x, y, z)| Shape::Box {
                min: CellPos::new(x, y, z),
                max: CellPos::new(x + 31, y + 7, z + 31),
            }),
            // Over MAX_EDIT_CELLS: rejected.
            1 => (cell_pos(), 20..=u16::MAX).prop_map(|(center, radius)| Shape::Sphere { center, radius }),
        ]
    }

    /// Ops over every material id plus one past the table (rejected).
    fn op_strategy(materials: u16) -> impl Strategy<Value = EditOp> {
        let damage = prop_oneof![8 => 1..=40u16, 1 => Just(0u16), 1 => any::<u16>()];
        prop_oneof![
            (0..=materials).prop_map(|m| EditOp::Set(MaterialId(m))),
            Just(EditOp::Remove),
            damage.prop_map(EditOp::Damage),
            (0..=materials).prop_map(|m| EditOp::Paint(MaterialId(m))),
        ]
    }

    /// An op, a shape and how many times in a row it is issued (repeats let
    /// damage accumulate to a break).
    fn command_strategy(materials: u16) -> impl Strategy<Value = (EditOp, Shape, u8)> {
        (
            op_strategy(materials),
            shape_strategy(),
            prop_oneof![3 => Just(1u8), 1 => 2..=16u8],
        )
    }

    /// Revision and `modified` of every resident chunk. Contents are
    /// compared through [`World::hash`], which folds every chunk hash.
    fn chunk_states(w: &World) -> Vec<(ChunkPos, u64, bool)> {
        w.iter_chunks_sorted()
            .map(|(p, c)| (p, c.revision, c.modified))
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(
            std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(256)
        ))]

        /// SAD §14: the same command sequence on two worlds gives the same
        /// world hash. The two worlds start with equal contents but insert
        /// their chunks in different orders (and own separately seeded hash
        /// maps) and hold structurally different storage. Every command must
        /// give them identical results (status, deltas, yields), and a
        /// replica fed only the deltas must hash the same at the end.
        #[test]
        fn determinism_proptest(
            specs in proptest::collection::vec(chunk_spec(13), DETERMINISM_CHUNKS),
            order in Just((0..DETERMINISM_CHUNKS).collect::<Vec<_>>()).prop_shuffle(),
            commands in proptest::collection::vec(command_strategy(13), 1..32),
        ) {
            let table = phase1();
            let materials = table.len() as u16;
            prop_assert_eq!(materials, 13, "strategies assume the 13-entry Phase 1 table");
            let spare = id(&table, "granite");
            let bounds = determinism_bounds();
            let positions: Vec<ChunkPos> = bounds.chunks().collect();
            prop_assert_eq!(positions.len(), DETERMINISM_CHUNKS);

            let mut a = World::new(table.clone(), bounds);
            for (&pos, spec) in positions.iter().zip(&specs) {
                if spec.0 {
                    a.insert_chunk(pos, build_chunk(spec)).unwrap();
                }
            }
            let mut b = World::new(table.clone(), bounds);
            for &i in &order {
                if specs[i].0 {
                    let chunk = restructure(build_chunk(&specs[i]), spare);
                    b.insert_chunk(positions[i], chunk).unwrap();
                }
            }
            prop_assert_eq!(a.hash(), b.hash(), "equal contents before any command");
            let mut replica = a.clone();

            let mut seq = 0;
            for &(op, shape, repeat) in &commands {
                for _ in 0..repeat {
                    seq += 1;
                    let c = cmd(seq, op, shape);
                    let ra = a.apply(&c);
                    let rb = b.apply(&c);
                    prop_assert_eq!(ra.command, c.id);
                    prop_assert_eq!(&ra, &rb, "command {:?}", c);
                    prop_assert!(
                        ra.deltas.windows(2).all(|d| d[0].pos() < d[1].pos()),
                        "deltas ascending by chunk"
                    );
                    prop_assert!(
                        ra.yields.windows(2).all(|y| y[0].0 < y[1].0),
                        "yields ascending by material"
                    );
                    replay_in_place(&mut replica, &ra.deltas);
                }
            }

            let hash = a.hash();
            prop_assert_eq!(b.hash(), hash);
            prop_assert_eq!(replica.hash(), hash);
            prop_assert_eq!(chunk_states(&a), chunk_states(&b));
            // `modified` is host-side save state, not part of a delta.
            let revisions = |w: &World| -> Vec<(ChunkPos, u64)> {
                chunk_states(w).into_iter().map(|(p, rev, _)| (p, rev)).collect()
            };
            prop_assert_eq!(revisions(&replica), revisions(&a));
            prop_assert_eq!(a.damage(), b.damage());
        }
    }

    // Phase 2: `SetLayer`, `ChunkDelta::Layers` and the displacement rules
    // (SAD-P2 §7.0, §7.0.1). Keys appear only in this fixture.

    use crate::coords::CHUNK_VOLUME;
    use crate::displace::EcoRules;
    use crate::layers::{
        LayerId, N_GASES, N_SUBSTANCES, PlantState, SWEEP_GAS, SWEEP_TRANSPORT, SWEEP_WATER,
    };
    use crate::ledger::SeasonLedger;
    use crate::world::DeltaError;

    const ECO_TABLE: &str = r#"[
        (id: 0, key: "air", phase: Air, tags: [], hardness: 0, diggable: false,
         yield_count: 0, color: (0, 0, 0, 0), emissive: 0.0),
        (id: 1, key: "stone", phase: Solid, tags: ["stone"], hardness: 30, diggable: true,
         yield_count: 1, color: (128, 128, 128, 255), emissive: 0.0),
        (id: 2, key: "soil", phase: Solid, tags: ["soil", "porous"], hardness: 10,
         diggable: true, yield_count: 1, color: (121, 85, 58, 255), emissive: 0.0,
         permeability: 32, field_capacity: 48, anchor: true),
        (id: 3, key: "water", phase: Liquid, tags: [], hardness: 0, diggable: true,
         yield_count: 0, color: (40, 80, 200, 128), emissive: 0.0),
        (id: 4, key: "ice", phase: Solid, tags: [], hardness: 5, diggable: true,
         yield_count: 0, color: (200, 220, 255, 200), emissive: 0.0),
        (id: 5, key: "magma", phase: Liquid, tags: [], hardness: 0, diggable: false,
         yield_count: 0, color: (255, 90, 0, 255), emissive: 2.0,
         viscosity: 3, conductivity: 64, heat_capacity: 32),
        (id: 6, key: "basalt", phase: Solid, tags: ["stone"], hardness: 60, diggable: true,
         yield_count: 1, color: (62, 62, 70, 255), emissive: 0.0),
        (id: 7, key: "moss", phase: Solid, tags: ["plant"], hardness: 1, diggable: true,
         yield_count: 1, color: (60, 140, 60, 255), emissive: 0.0, passable: true),
        (id: 8, key: "copper_dust", phase: Solid, tags: ["placeable"], hardness: 5,
         diggable: true, yield_count: 1, color: (184, 115, 51, 255), emissive: 0.0),
    ]"#;

    const STEAM: u8 = 0;
    const FIREDAMP: u8 = 1;
    const CU: u8 = 0;
    const ORGANIC: u8 = 9;
    const MAGMA_INIT: i16 = 12000;
    const LITTER: u8 = 8;
    const HARVEST_SHIFT: u8 = 3;

    fn eco_table() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::from_ron_str(ECO_TABLE).expect("eco test table loads"))
    }

    fn eco_rules(t: &MaterialTable) -> EcoRules {
        let mut substance_yields = [None; N_SUBSTANCES];
        substance_yields[usize::from(CU)] = Some(id(t, "copper_dust"));
        EcoRules {
            water: Some(id(t, "water")),
            ice: Some(id(t, "ice")),
            magma: Some(id(t, "magma")),
            steam: Some(STEAM),
            organic: Some(ORGANIC),
            magma_init_temp: MAGMA_INIT,
            plant_litter: vec![LITTER],
            harvest_shift: HARVEST_SHIFT,
            substance_yields,
        }
    }

    /// Two resident air chunks (cells 0..=63 × 0..=31 × 0..=31) with the
    /// fixture's eco rules.
    fn eco_world() -> World {
        let table = eco_table();
        let rules = eco_rules(&table);
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 31));
        let mut w = World::new(table, bounds);
        for pos in bounds.chunks() {
            w.insert_chunk(pos, Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        w.set_eco_rules(rules);
        w
    }

    fn at(x: i32, y: i32, z: i32) -> CellPos {
        CellPos::new(x, y, z)
    }

    fn mat(w: &World, key: &str) -> MaterialId {
        id(w.materials(), key)
    }

    /// Conserved totals (SAD-P2 §7): water counts moisture on every cell plus
    /// steam; substances count layers plus plant `held`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Totals {
        water: i64,
        subs: [i64; N_SUBSTANCES],
        gases: [i64; N_GASES],
        magma: i64,
    }

    fn totals(w: &World) -> Totals {
        let magma = w.eco_rules().magma;
        let mut t = Totals {
            water: 0,
            subs: [0; N_SUBSTANCES],
            gases: [0; N_GASES],
            magma: 0,
        };
        for (_, chunk) in w.iter_chunks_sorted() {
            for i in 0..CHUNK_VOLUME {
                let local = LocalPos::from_index(i as u16);
                t.water += i64::from(chunk.layers.get_u8(LayerId::Moisture, local));
                for (x, s) in t.subs.iter_mut().enumerate() {
                    *s += i64::from(chunk.layers.get_u8(LayerId::Substance(x as u8), local));
                }
                for (g, s) in t.gases.iter_mut().enumerate() {
                    *s += i64::from(chunk.layers.get_u8(LayerId::Gas(g as u8), local));
                }
                if Some(chunk.storage.get(local)) == magma {
                    t.magma += 1;
                }
            }
            for plant in chunk.plants.values() {
                for (x, s) in t.subs.iter_mut().enumerate() {
                    *s += i64::from(plant.held[x]);
                }
            }
        }
        t.water += t.gases[usize::from(STEAM)];
        t
    }

    /// The SAD-P2 §7 invariants applied to `before` and a ledger, with the
    /// two terms the ledger adds (`water_saturation_loss`, `gas_edit_in`).
    fn expected(before: Totals, l: &SeasonLedger) -> Totals {
        let mut t = before;
        t.water += l.rain_in + l.water_edit_in
            - l.water_edit_out
            - l.steam_vent_out
            - l.water_saturation_loss;
        for x in 0..N_SUBSTANCES {
            t.subs[x] += l.weather_in[x] + l.litter_in[x] + l.edit_in[x]
                - l.edit_out[x]
                - l.uptake_sink[x]
                - l.harvest_out[x]
                + l.react_delta[x]
                - l.saturation_loss[x];
        }
        for g in 0..N_GASES {
            let (emit, vent, react_out) = if g == usize::from(STEAM) {
                (
                    l.evap_to_steam + l.boil_to_steam,
                    l.steam_vent_out,
                    l.react_out[g] + l.condense_from_steam,
                )
            } else {
                (l.emit_in[g], l.vent_out[g], l.react_out[g])
            };
            t.gases[g] += emit + l.react_in[g] + l.gas_edit_in[g]
                - vent
                - react_out
                - l.gas_edit_out[g]
                - l.gas_saturation_loss[g];
        }
        t.magma += l.magma_melt_in + l.magma_edit - l.magma_set;
        t
    }

    /// Replays every delta of an edit onto a copy of `before`, as a replica.
    fn replay_all(before: &World, deltas: &[ChunkDelta]) -> World {
        let mut w = before.clone();
        for d in deltas {
            w.apply_delta(d)
                .unwrap_or_else(|e| panic!("delta {d:?}: {e}"));
        }
        w
    }

    /// Applies `c`, which must be applied, and checks the invariants and
    /// that its deltas reproduce it on a replica.
    fn edit_checked(w: &mut World, c: EditCommand) -> EditResult {
        let before = w.clone();
        let t0 = totals(w);
        let r = w.apply(&c);
        assert_eq!(r.status, EditStatus::Applied, "{:?}", c.op);
        assert_eq!(
            totals(w),
            expected(t0, &r.ledger),
            "invariants after {:?}",
            c.op
        );
        assert_eq!(
            replay_all(&before, &r.deltas).hash(),
            w.hash(),
            "replica after {:?}",
            c.op
        );
        r
    }

    /// `eco_set` with the invariants checked; returns the pass ledger.
    fn eco_checked(w: &mut World, cell: CellPos, to: MaterialId) -> SeasonLedger {
        let t0 = totals(w);
        let mut ledger = SeasonLedger::default();
        w.eco_set(cell, to, &mut ledger);
        assert_eq!(totals(w), expected(t0, &ledger), "invariants after eco_set");
        ledger
    }

    fn cell_shape(c: CellPos) -> Shape {
        Shape::Cell(c)
    }

    fn set_at(w: &mut World, seq: u32, key: &str, cells: &[CellPos]) {
        let m = mat(w, key);
        for &c in cells {
            let r = w.apply(&cmd(seq, EditOp::Set(m), cell_shape(c)));
            assert_eq!(r.status, EditStatus::Applied);
        }
    }

    fn layer_at(w: &mut World, seq: u32, c: CellPos, layer: LayerId, value: i32) {
        let r = w.apply(&cmd(seq, EditOp::SetLayer { layer, value }, cell_shape(c)));
        assert_eq!(r.status, EditStatus::Applied);
        assert_eq!(w.layer(c, layer), value, "{layer:?} at {c:?}");
    }

    fn put_plant(w: &mut World, c: CellPos, plant: PlantState) {
        let (pos, local) = c.split();
        w.chunks
            .get_mut(&pos)
            .unwrap()
            .plants
            .insert(local.index(), plant);
    }

    fn held(x: u8, amount: u8) -> PlantState {
        let mut p = PlantState::default();
        p.held[usize::from(x)] = amount;
        p
    }

    fn chunk_revs(w: &World, pos: ChunkPos) -> (u64, u64, bool) {
        let c = w.chunk(pos).unwrap();
        (c.revision, c.eco_revision, c.modified)
    }

    fn active(w: &World, c: CellPos, sweep: usize) -> bool {
        let (pos, local) = c.split();
        w.chunk(pos).unwrap().active[sweep].contains(local)
    }

    /// The five stone cells around `c` other than the one at `open`.
    fn enclose_except(w: &mut World, seq: u32, c: CellPos, open: Option<CellPos>) {
        let p = c.0;
        let around = [
            at(p.x, p.y - 1, p.z),
            at(p.x, p.y, p.z - 1),
            at(p.x - 1, p.y, p.z),
            at(p.x + 1, p.y, p.z),
            at(p.x, p.y, p.z + 1),
            at(p.x, p.y + 1, p.z),
        ];
        let walls: Vec<CellPos> = around.into_iter().filter(|&n| Some(n) != open).collect();
        set_at(w, seq, "stone", &walls);
    }

    #[test]
    fn set_layer_emits_layers_delta_and_bumps_eco_revision() {
        let mut w = eco_world();
        let stone = mat(&w, "stone");
        let (c0, c1) = (ChunkPos::new(0, 0, 0), ChunkPos::new(1, 0, 0));
        // A stone floor at y = 0..=1, x = 30..=33, z = 0.
        w.apply(&cmd(1, EditOp::Set(stone), boxed((30, 0, 0), (33, 1, 0))));
        let (rev0, rev1) = (chunk_revs(&w, c0).0, chunk_revs(&w, c1).0);
        assert_eq!(chunk_revs(&w, c0).1, 0);

        // Moisture 40 on a 4 × 2 × 1 box straddling x = 31 | 32: stone at
        // y = 1, air at y = 2.
        let moisture = |value| EditOp::SetLayer {
            layer: LayerId::Moisture,
            value,
        };
        let r = edit_checked(&mut w, cmd(2, moisture(40), boxed((30, 1, 0), (33, 2, 0))));
        assert!(r.yields.is_empty());
        assert_eq!(r.deltas.len(), 2, "{:?}", r.deltas);
        for (d, (pos, xs)) in r.deltas.iter().zip([(c0, [30u32, 31]), (c1, [0, 1])]) {
            let ChunkDelta::Layers {
                pos: p,
                eco_revision,
                changes,
                plants,
            } = d
            else {
                panic!("expected a Layers delta, got {d:?}");
            };
            assert_eq!((*p, *eco_revision), (pos, 1));
            assert!(plants.is_empty());
            let mut want = Vec::new();
            for y in 1..=2 {
                for x in xs {
                    want.push((LocalPos::new(x, y, 0).index(), LayerId::Moisture, 40));
                }
            }
            assert_eq!(changes, &want);
        }
        let mut ledger = SeasonLedger {
            water_edit_in: 8 * 40,
            ..SeasonLedger::default()
        };
        assert_eq!(*r.ledger, ledger);
        // Materials and revisions are untouched; the eco revision bumped.
        assert_eq!(chunk_revs(&w, c0), (rev0, 1, true));
        assert_eq!(chunk_revs(&w, c1), (rev1, 1, true));
        assert_eq!(w.get_cell(at(30, 1, 0)), Some(stone));
        assert_eq!(w.layer(at(33, 2, 0), LayerId::Moisture), 40);

        // The same value again changes nothing and emits nothing.
        let r = edit_checked(&mut w, cmd(3, moisture(40), boxed((30, 1, 0), (33, 2, 0))));
        assert!(r.deltas.is_empty());
        assert_eq!(*r.ledger, SeasonLedger::default());
        assert_eq!(chunk_revs(&w, c0).1, 1);

        // Lowering it is an edit sink.
        let r = edit_checked(&mut w, cmd(4, moisture(10), boxed((30, 1, 0), (31, 1, 0))));
        assert_eq!(r.deltas.len(), 1);
        assert_eq!(r.deltas[0].revision(), 2);
        ledger = SeasonLedger {
            water_edit_out: 2 * 30,
            ..SeasonLedger::default()
        };
        assert_eq!(*r.ledger, ledger);

        // Gas goes on air cells only: of x = 30..=33, y = 0..=2 only y = 2
        // is air.
        let r = edit_checked(
            &mut w,
            cmd(
                5,
                EditOp::SetLayer {
                    layer: LayerId::Gas(FIREDAMP),
                    value: 50,
                },
                boxed((30, 0, 0), (33, 2, 0)),
            ),
        );
        let mut gas_in = [0; N_GASES];
        gas_in[usize::from(FIREDAMP)] = 4 * 50;
        assert_eq!(r.ledger.gas_edit_in, gas_in);
        assert_eq!(w.layer(at(30, 1, 0), LayerId::Gas(FIREDAMP)), 0);
        assert_eq!(w.layer(at(30, 2, 0), LayerId::Gas(FIREDAMP)), 50);
        assert!(active(&w, at(30, 2, 0), SWEEP_GAS + usize::from(FIREDAMP)));

        // Steam is water too.
        let r = edit_checked(
            &mut w,
            cmd(
                6,
                EditOp::SetLayer {
                    layer: LayerId::Gas(STEAM),
                    value: 7,
                },
                cell_shape(at(5, 5, 5)),
            ),
        );
        assert_eq!(r.ledger.gas_edit_in[usize::from(STEAM)], 7);
        assert_eq!(r.ledger.water_edit_in, 7);

        // Temperature is an i16 layer and no ledger term.
        let r = edit_checked(
            &mut w,
            cmd(
                7,
                EditOp::SetLayer {
                    layer: LayerId::Temperature,
                    value: -300,
                },
                cell_shape(at(31, 0, 0)),
            ),
        );
        assert_eq!(*r.ledger, SeasonLedger::default());
        assert_eq!(w.layer(at(31, 0, 0), LayerId::Temperature), -300);
        let ChunkDelta::Layers { changes, .. } = &r.deltas[0] else {
            panic!("expected Layers");
        };
        assert_eq!(
            changes,
            &[(LocalPos::new(31, 0, 0).index(), LayerId::Temperature, -300)]
        );

        // Values that do not fit, and ids out of range, reject the command.
        let hash = w.hash();
        for (layer, value) in [
            (LayerId::Moisture, 256),
            (LayerId::Moisture, -1),
            (LayerId::Temperature, 40_000),
            (LayerId::Substance(N_SUBSTANCES as u8), 1),
            (LayerId::Gas(N_GASES as u8), 1),
        ] {
            let r = w.apply(&cmd(
                8,
                EditOp::SetLayer { layer, value },
                cell_shape(at(1, 1, 1)),
            ));
            assert_eq!(
                r.status,
                EditStatus::Rejected(RejectReason::InvalidLayerValue { layer, value })
            );
            assert!(r.deltas.is_empty());
        }
        assert_eq!(w.hash(), hash);

        // A replica refuses a Layers delta that skips an eco revision or
        // carries a value outside its layer's type, and changes nothing.
        let mut replica = w.clone();
        let eco = chunk_revs(&w, c0).1;
        let gap = ChunkDelta::Layers {
            pos: c0,
            eco_revision: eco + 2,
            changes: vec![(0, LayerId::Moisture, 1)],
            plants: Vec::new(),
        };
        assert_eq!(
            replica.apply_delta(&gap),
            Err(DeltaError::RevisionGap {
                pos: c0,
                local: eco,
                got: eco + 2
            })
        );
        let bad = ChunkDelta::Layers {
            pos: c0,
            eco_revision: eco + 1,
            changes: vec![(0, LayerId::Moisture, 1), (1, LayerId::Moisture, 300)],
            plants: Vec::new(),
        };
        assert_eq!(
            replica.apply_delta(&bad),
            Err(DeltaError::BadLayerChange {
                pos: c0,
                index: 1,
                layer: LayerId::Moisture,
                value: 300
            })
        );
        let bad_plant = ChunkDelta::Layers {
            pos: c0,
            eco_revision: eco + 1,
            changes: Vec::new(),
            plants: vec![(CHUNK_VOLUME as u16, None)],
        };
        assert_eq!(
            replica.apply_delta(&bad_plant),
            Err(DeltaError::BadIndex {
                pos: c0,
                index: CHUNK_VOLUME as u16
            })
        );
        assert_eq!(replica.hash(), w.hash());
        assert_eq!(chunk_revs(&replica, c0).1, eco);
    }

    #[test]
    fn displacement_rules_every_case_ledgered() {
        let mut w = eco_world();
        let (stone, water, magma, basalt, moss) = (
            mat(&w, "stone"),
            mat(&w, "water"),
            mat(&w, "magma"),
            mat(&w, "basalt"),
            mat(&w, "moss"),
        );
        let copper_dust = mat(&w, "copper_dust");
        let (c0, c1) = (ChunkPos::new(0, 0, 0), ChunkPos::new(1, 0, 0));
        let cu = LayerId::Substance(CU);
        let organic = LayerId::Substance(ORGANIC);
        let (steam, firedamp) = (LayerId::Gas(STEAM), LayerId::Gas(FIREDAMP));

        // Solid → air (brush Remove): moisture and substances stay on the
        // cell; regolith and growth zero.
        let a = at(10, 10, 10);
        set_at(&mut w, 1, "stone", &[a]);
        for (layer, v) in [
            (LayerId::Moisture, 30),
            (cu, 20),
            (LayerId::Regolith, 100),
            (LayerId::Growth, 5),
        ] {
            layer_at(&mut w, 2, a, layer, v);
        }
        let r = edit_checked(&mut w, cmd(3, EditOp::Remove, cell_shape(a)));
        assert_eq!(*r.ledger, SeasonLedger::default());
        assert_eq!(w.get_cell(a), Some(MaterialId::AIR));
        assert_eq!(
            [LayerId::Moisture, cu, LayerId::Regolith, LayerId::Growth].map(|l| w.layer(a, l)),
            [30, 20, 0, 0]
        );
        // The material delta comes first, then the layer delta.
        assert!(matches!(
            &r.deltas[..],
            [ChunkDelta::Cells { .. }, ChunkDelta::Layers { pos, .. }] if *pos == c0
        ));

        // Water → air by the pickaxe: the fill stays (a puddle).
        let b = at(12, 10, 10);
        set_at(&mut w, 4, "water", &[b]);
        layer_at(&mut w, 5, b, LayerId::Moisture, 90);
        let r = edit_checked(&mut w, cmd(6, EditOp::Damage(u16::MAX), cell_shape(b)));
        assert_eq!(*r.ledger, SeasonLedger::default());
        assert_eq!(w.get_cell(b), Some(MaterialId::AIR));
        assert_eq!(w.layer(b, LayerId::Moisture), 90);

        // Air → solid: moisture goes to the first air or water neighbor in
        // sorted order with room (here the one below, room 55); substances
        // follow proportionally (50 · 55 / 100 = 27); gases fill the air
        // neighbors (only the one below); the rest is an edit sink, and
        // lost steam is lost water.
        let c = at(20, 10, 10);
        let below = at(20, 9, 10);
        enclose_except(&mut w, 7, c, Some(below));
        for (cell, layer, v) in [
            (c, LayerId::Moisture, 100),
            (c, cu, 50),
            (c, firedamp, 200),
            (c, steam, 20),
            (below, LayerId::Moisture, 200),
            (below, firedamp, 250),
            (below, steam, 250),
        ] {
            layer_at(&mut w, 8, cell, layer, v);
        }
        let r = edit_checked(&mut w, cmd(9, EditOp::Set(stone), cell_shape(c)));
        let mut want = SeasonLedger {
            water_edit_out: 45 + 15,
            ..SeasonLedger::default()
        };
        want.gas_edit_out[usize::from(FIREDAMP)] = 195;
        want.gas_edit_out[usize::from(STEAM)] = 15;
        assert_eq!(*r.ledger, want);
        assert_eq!(
            [LayerId::Moisture, cu, firedamp, steam].map(|l| w.layer(c, l)),
            [0, 23, 0, 0]
        );
        assert_eq!(
            [LayerId::Moisture, cu, firedamp, steam].map(|l| w.layer(below, l)),
            [255, 27, 255, 255]
        );
        assert!(active(&w, below, SWEEP_WATER));

        // The push crosses a chunk border: the receiver's chunk gets its own
        // Layers delta, after the source chunk's deltas.
        let d = at(31, 20, 20);
        let east = at(32, 20, 20);
        enclose_except(&mut w, 10, d, Some(east));
        layer_at(&mut w, 11, d, LayerId::Moisture, 60);
        let eco1 = chunk_revs(&w, c1).1;
        let r = edit_checked(&mut w, cmd(12, EditOp::Set(stone), cell_shape(d)));
        assert_eq!(*r.ledger, SeasonLedger::default());
        assert!(matches!(
            &r.deltas[..],
            [
                ChunkDelta::Cells { pos: p0, .. },
                ChunkDelta::Layers { pos: p1, .. },
                ChunkDelta::Layers { pos: p2, .. },
            ] if *p0 == c0 && *p1 == c0 && *p2 == c1
        ));
        assert_eq!(w.layer(east, LayerId::Moisture), 60);
        assert_eq!(chunk_revs(&w, c1).1, eco1 + 1);
        assert!(active(&w, east, SWEEP_WATER));

        // Air → water by the brush: gases are pushed (to the air below), and
        // the fill becomes 255.
        let e = at(40, 10, 10);
        layer_at(&mut w, 13, e, firedamp, 100);
        layer_at(&mut w, 14, e, LayerId::Moisture, 30);
        let r = edit_checked(&mut w, cmd(15, EditOp::Set(water), cell_shape(e)));
        assert_eq!(
            *r.ledger,
            SeasonLedger {
                water_edit_in: 225,
                ..SeasonLedger::default()
            }
        );
        assert_eq!(w.layer(e, LayerId::Moisture), 255);
        assert_eq!(w.layer(e, firedamp), 0);
        assert_eq!(w.layer(at(40, 9, 10), firedamp), 100);
        assert!(active(&w, e, SWEEP_WATER));

        // Stone → magma by the brush: magma_edit, temperature set; magma →
        // basalt: magma_edit back, temperature unchanged.
        let f = at(14, 10, 10);
        set_at(&mut w, 16, "stone", &[f]);
        let r = edit_checked(&mut w, cmd(17, EditOp::Set(magma), cell_shape(f)));
        assert_eq!(
            *r.ledger,
            SeasonLedger {
                magma_edit: 1,
                ..SeasonLedger::default()
            }
        );
        assert_eq!(w.layer(f, LayerId::Temperature), i32::from(MAGMA_INIT));
        layer_at(&mut w, 18, f, LayerId::Temperature, 9500);
        let r = edit_checked(&mut w, cmd(19, EditOp::Set(basalt), cell_shape(f)));
        assert_eq!(
            *r.ledger,
            SeasonLedger {
                magma_edit: -1,
                ..SeasonLedger::default()
            }
        );
        assert_eq!(w.layer(f, LayerId::Temperature), 9500);

        // Digging a plant harvests it: held substances leave as
        // harvest_out, the yield material comes out as max(1, held >> 3),
        // and a tall plant's upper cell goes with it.
        let (anchor, plant, top) = (at(50, 10, 10), at(50, 11, 10), at(50, 12, 10));
        set_at(&mut w, 20, "soil", &[anchor]);
        set_at(&mut w, 21, "moss", &[plant, top]);
        put_plant(&mut w, plant, held(CU, 40));
        let r = edit_checked(&mut w, cmd(22, EditOp::Damage(u16::MAX), cell_shape(plant)));
        let mut want = SeasonLedger::default();
        want.harvest_out[usize::from(CU)] = 40;
        assert_eq!(*r.ledger, want);
        assert_eq!(
            r.yields,
            vec![(moss, 1), (copper_dust, 40 >> HARVEST_SHIFT)]
        );
        assert_eq!(w.get_cell(plant), Some(MaterialId::AIR));
        assert_eq!(w.get_cell(top), Some(MaterialId::AIR));
        assert!(w.plant(plant).is_none());
        let removed = r.deltas.iter().any(|d| {
            matches!(d, ChunkDelta::Layers { plants, .. }
                if plants == &[(plant.local().index(), None)])
        });
        assert!(
            removed,
            "plant removal travels in the Layers delta: {:?}",
            r.deltas
        );

        // Removing a plant without harvest returns `held` to the anchor below
        // by room; the rest is an edit sink.
        let (anchor, plant) = (at(52, 10, 10), at(52, 11, 10));
        set_at(&mut w, 23, "soil", &[anchor]);
        set_at(&mut w, 24, "moss", &[plant]);
        layer_at(&mut w, 25, anchor, cu, 250);
        put_plant(&mut w, plant, held(CU, 40));
        let r = edit_checked(&mut w, cmd(26, EditOp::Remove, cell_shape(plant)));
        let mut want = SeasonLedger::default();
        want.edit_out[usize::from(CU)] = 35;
        assert_eq!(*r.ledger, want);
        assert!(r.yields.is_empty());
        assert_eq!(w.layer(anchor, cu), 255);

        // Over a non-anchor, everything held is lost.
        let (base, plant) = (at(56, 10, 10), at(56, 11, 10));
        set_at(&mut w, 27, "stone", &[base]);
        set_at(&mut w, 28, "moss", &[plant]);
        put_plant(&mut w, plant, held(CU, 12));
        let r = edit_checked(&mut w, cmd(29, EditOp::Set(stone), cell_shape(plant)));
        let mut want = SeasonLedger::default();
        want.edit_out[usize::from(CU)] = 12;
        assert_eq!(*r.ledger, want);

        // Digging the anchor kills the plant on top by the death rule:
        // litter and held go into the cell below the plant, the plant cell
        // becomes air, and plants_died counts it.
        let (anchor, plant) = (at(54, 10, 10), at(54, 11, 10));
        set_at(&mut w, 30, "soil", &[anchor]);
        set_at(&mut w, 31, "moss", &[plant]);
        put_plant(&mut w, plant, held(CU, 10));
        let r = edit_checked(&mut w, cmd(32, EditOp::Remove, cell_shape(anchor)));
        let mut want = SeasonLedger {
            plants_died: 1,
            ..SeasonLedger::default()
        };
        want.litter_in[usize::from(ORGANIC)] = i64::from(LITTER);
        assert_eq!(*r.ledger, want);
        assert_eq!(w.get_cell(anchor), Some(MaterialId::AIR));
        assert_eq!(w.get_cell(plant), Some(MaterialId::AIR));
        assert!(w.plant(plant).is_none());
        assert_eq!(w.layer(anchor, organic), i32::from(LITTER));
        assert_eq!(w.layer(anchor, cu), 10);
        let ChunkDelta::Cells { changes, .. } = &r.deltas[0] else {
            panic!("expected Cells first: {:?}", r.deltas);
        };
        assert_eq!(
            changes,
            &[
                (anchor.local().index(), MaterialId::AIR),
                (plant.local().index(), MaterialId::AIR),
            ]
        );
        assert_eq!(w.get_cell(at(54, 9, 10)), Some(MaterialId::AIR));
    }

    #[test]
    fn brush_set_water_fill_255_set_magma_init_temp() {
        let mut w = eco_world();
        let (water, ice, magma) = (mat(&w, "water"), mat(&w, "ice"), mat(&w, "magma"));

        // Set(water) over air: every cell is full, and the ledger records
        // what was added on top of the moisture already there.
        layer_at(&mut w, 1, at(5, 5, 5), LayerId::Moisture, 40);
        let r = edit_checked(
            &mut w,
            cmd(2, EditOp::Set(water), boxed((5, 5, 5), (6, 5, 6))),
        );
        assert_eq!(r.ledger.water_edit_in, 4 * 255 - 40);
        for c in boxed((5, 5, 5), (6, 5, 6)).cells() {
            assert_eq!(w.get_cell(c), Some(water));
            assert_eq!(w.layer(c, LayerId::Moisture), 255, "{c:?}");
            assert!(active(&w, c, SWEEP_WATER));
        }

        // Set(ice) over a damp soil cell: full too.
        let s = at(10, 5, 5);
        set_at(&mut w, 3, "soil", &[s]);
        layer_at(&mut w, 4, s, LayerId::Moisture, 30);
        let r = edit_checked(&mut w, cmd(5, EditOp::Set(ice), cell_shape(s)));
        assert_eq!(
            *r.ledger,
            SeasonLedger {
                water_edit_in: 225,
                ..SeasonLedger::default()
            }
        );
        assert_eq!(w.layer(s, LayerId::Moisture), 255);

        // A cell already holding water is skipped, as every Set skips a
        // cell that holds its target.
        let p = at(5, 5, 5);
        layer_at(&mut w, 6, p, LayerId::Moisture, 100);
        let r = edit_checked(&mut w, cmd(7, EditOp::Set(water), cell_shape(p)));
        assert!(r.deltas.is_empty());
        assert_eq!(*r.ledger, SeasonLedger::default());
        assert_eq!(w.layer(p, LayerId::Moisture), 100);

        // Set(magma) over stone: temperature eco.magma_init_temp, one
        // magma_edit per cell.
        set_at(
            &mut w,
            8,
            "stone",
            &[at(20, 5, 5), at(21, 5, 5), at(22, 5, 5)],
        );
        let r = edit_checked(
            &mut w,
            cmd(9, EditOp::Set(magma), boxed((20, 5, 5), (22, 5, 5))),
        );
        assert_eq!(
            *r.ledger,
            SeasonLedger {
                magma_edit: 3,
                ..SeasonLedger::default()
            }
        );
        for x in 20..=22 {
            let c = at(x, 5, 5);
            assert_eq!(w.get_cell(c), Some(magma));
            assert_eq!(w.layer(c, LayerId::Temperature), i32::from(MAGMA_INIT));
            assert!(active(&w, c, SWEEP_WATER));
        }

        // Set(magma) over water: magma holds no moisture, so the fill goes
        // to the air below.
        let q = at(6, 5, 6);
        let r = edit_checked(&mut w, cmd(10, EditOp::Set(magma), cell_shape(q)));
        assert_eq!(
            *r.ledger,
            SeasonLedger {
                magma_edit: 1,
                ..SeasonLedger::default()
            }
        );
        assert_eq!(w.layer(q, LayerId::Moisture), 0);
        assert_eq!(w.layer(at(6, 4, 6), LayerId::Moisture), 255);
        assert_eq!(w.layer(q, LayerId::Temperature), i32::from(MAGMA_INIT));
    }

    #[test]
    fn eco_set_displaces_bumps_revision_no_delta() {
        let mut w = eco_world();
        let (stone, water, ice, magma, basalt) = (
            mat(&w, "stone"),
            mat(&w, "water"),
            mat(&w, "ice"),
            mat(&w, "magma"),
            mat(&w, "basalt"),
        );
        let (c0, c1) = (ChunkPos::new(0, 0, 0), ChunkPos::new(1, 0, 0));
        let cu = LayerId::Substance(CU);
        let (steam, firedamp) = (LayerId::Gas(STEAM), LayerId::Gas(FIREDAMP));

        // Boiling: water → air turns the fill into steam on the cell.
        let a = at(10, 10, 10);
        set_at(&mut w, 1, "water", &[a]);
        layer_at(&mut w, 2, a, LayerId::Moisture, 200);
        assert_eq!(w.surface_y(10, 10), 10);
        let (rev, eco, _) = chunk_revs(&w, c0);
        let l = eco_checked(&mut w, a, MaterialId::AIR);
        assert_eq!(
            l,
            SeasonLedger {
                boil_to_steam: 200,
                ..SeasonLedger::default()
            }
        );
        assert_eq!(w.get_cell(a), Some(MaterialId::AIR));
        assert_eq!(w.layer(a, LayerId::Moisture), 0);
        assert_eq!(w.layer(a, steam), 200);
        assert_eq!(chunk_revs(&w, c0), (rev + 1, eco + 1, true));
        assert_eq!(w.surface_y(10, 10), crate::columns::NO_SURFACE);
        assert!(active(&w, a, SWEEP_GAS + usize::from(STEAM)));

        // Air → solid with nowhere to go: pass remainders are saturation
        // losses, substances stay adsorbed.
        let b = at(20, 10, 10);
        enclose_except(&mut w, 3, b, None);
        for (layer, v) in [(LayerId::Moisture, 70), (firedamp, 30), (steam, 5), (cu, 9)] {
            layer_at(&mut w, 4, b, layer, v);
        }
        let l = eco_checked(&mut w, b, stone);
        let mut want = SeasonLedger {
            water_saturation_loss: 70 + 5,
            ..SeasonLedger::default()
        };
        want.gas_saturation_loss[usize::from(FIREDAMP)] = 30;
        want.gas_saturation_loss[usize::from(STEAM)] = 5;
        assert_eq!(l, want);
        assert_eq!(w.layer(b, cu), 9);
        assert_eq!(w.surface_y(20, 10), 11);

        // Water ↔ ice keeps the moisture.
        let c = at(30, 5, 5);
        set_at(&mut w, 5, "water", &[c]);
        layer_at(&mut w, 6, c, LayerId::Moisture, 120);
        assert_eq!(eco_checked(&mut w, c, ice), SeasonLedger::default());
        assert_eq!(w.layer(c, LayerId::Moisture), 120);
        assert_eq!(eco_checked(&mut w, c, water), SeasonLedger::default());
        assert_eq!(w.layer(c, LayerId::Moisture), 120);

        // Stone → magma (melt) and magma → basalt (set): counted, and the
        // temperature is the pass's business.
        let d = at(12, 5, 5);
        set_at(&mut w, 7, "stone", &[d]);
        layer_at(&mut w, 8, d, LayerId::Temperature, 12600);
        let l = eco_checked(&mut w, d, magma);
        assert_eq!(
            l,
            SeasonLedger {
                magma_melt_in: 1,
                ..SeasonLedger::default()
            }
        );
        assert_eq!(w.layer(d, LayerId::Temperature), 12600);
        assert!(active(&w, d, SWEEP_WATER));
        let l = eco_checked(&mut w, d, basalt);
        assert_eq!(
            l,
            SeasonLedger {
                magma_set: 1,
                ..SeasonLedger::default()
            }
        );

        // A push into the next chunk bumps that chunk's eco revision only.
        let e = at(31, 20, 20);
        let east = at(32, 20, 20);
        enclose_except(&mut w, 9, e, Some(east));
        layer_at(&mut w, 10, e, LayerId::Moisture, 60);
        let (rev0, _, _) = chunk_revs(&w, c0);
        let (rev1, eco1, _) = chunk_revs(&w, c1);
        assert_eq!(eco_checked(&mut w, e, stone), SeasonLedger::default());
        assert_eq!(w.layer(east, LayerId::Moisture), 60);
        assert_eq!(chunk_revs(&w, c0).0, rev0 + 1);
        assert_eq!(chunk_revs(&w, c1).0, rev1);
        assert_eq!(chunk_revs(&w, c1).1, eco1 + 1);
        assert!(active(&w, east, SWEEP_WATER));

        // The cell's damage is cleared.
        let f = at(60, 5, 5);
        set_at(&mut w, 11, "stone", &[f]);
        w.apply(&cmd(12, EditOp::Damage(1), cell_shape(f)));
        assert_eq!(w.damage().get(f), 1);
        eco_checked(&mut w, f, basalt);
        assert_eq!(w.damage().get(f), 0);

        // A plant removed by a pass returns `held` to its anchor; no harvest.
        let (anchor, plant) = (at(40, 10, 10), at(40, 11, 10));
        set_at(&mut w, 13, "soil", &[anchor]);
        set_at(&mut w, 14, "moss", &[plant]);
        put_plant(&mut w, plant, held(CU, 10));
        assert_eq!(
            eco_checked(&mut w, plant, MaterialId::AIR),
            SeasonLedger::default()
        );
        assert_eq!(w.layer(anchor, cu), 10);
        assert!(w.plant(plant).is_none());

        // from == to and cells outside the bounds change nothing.
        let hash = w.hash();
        let revs: Vec<_> = [c0, c1].map(|p| chunk_revs(&w, p)).to_vec();
        assert_eq!(eco_checked(&mut w, f, basalt), SeasonLedger::default());
        let mut l = SeasonLedger::default();
        w.eco_set(at(-1, 0, 0), stone, &mut l);
        w.eco_set(at(64, 0, 0), stone, &mut l);
        assert_eq!(l, SeasonLedger::default());
        assert_eq!(w.hash(), hash);
        assert_eq!([c0, c1].map(|p| chunk_revs(&w, p)).to_vec(), revs);
    }

    #[test]
    fn eco_put_layer_and_plant_bump_eco_revision_only() {
        let mut w = eco_world();
        let c0 = ChunkPos::new(0, 0, 0);
        let a = at(4, 20, 4);
        let (rev, eco, modified) = chunk_revs(&w, c0);
        assert!(w.eco_put_layer(a, LayerId::Moisture, 9));
        assert!(!w.eco_put_layer(a, LayerId::Moisture, 9), "unchanged");
        assert_eq!(chunk_revs(&w, c0), (rev, eco + 1, modified));
        assert!(active(&w, a, SWEEP_WATER), "moisture joins the water sweep");
        assert!(w.eco_put_layer(a, LayerId::Gas(FIREDAMP), 3));
        assert!(active(&w, a, SWEEP_GAS + usize::from(FIREDAMP)));
        assert!(w.eco_put_layer(a, LayerId::Temperature, -40));
        assert_eq!(w.layer(a, LayerId::Temperature), -40);

        let plant = crate::layers::PlantState {
            species: 1,
            ..Default::default()
        };
        assert!(w.eco_put_plant(a, Some(plant)));
        assert!(!w.eco_put_plant(a, Some(plant)));
        assert_eq!(w.plant(a), Some(&plant));
        assert!(w.eco_put_plant(a, None));
        assert_eq!(w.plant(a), None);
        assert_eq!(chunk_revs(&w, c0), (rev, eco + 5, modified));

        // Activation is explicit and per kind; out of bounds never activates.
        assert!(w.eco_activate(a, SWEEP_TRANSPORT));
        assert!(!w.eco_activate(a, SWEEP_TRANSPORT));
        assert!(w.eco_deactivate(a, SWEEP_TRANSPORT));
        assert!(!w.eco_deactivate(a, SWEEP_TRANSPORT));
        assert!(!w.eco_activate(at(-1, 0, 0), SWEEP_TRANSPORT));
        assert!(!w.eco_put_layer(at(-1, 0, 0), LayerId::Moisture, 1));
    }

    #[test]
    fn boil_without_steam_room_loses_water_not_steam() {
        let mut w = eco_world();
        let a = at(10, 10, 10);
        set_at(&mut w, 1, "water", &[a]);
        layer_at(&mut w, 2, a, LayerId::Moisture, 200);
        // Steam already on the cell leaves room for 155 of the fill.
        assert!(w.eco_put_layer(a, LayerId::Gas(STEAM), 100));
        let l = eco_checked(&mut w, a, MaterialId::AIR);
        assert_eq!(l.boil_to_steam, 155);
        assert_eq!(l.water_saturation_loss, 45);
        assert_eq!(l.gas_saturation_loss, [0; N_GASES]);
        assert_eq!(w.layer(a, LayerId::Gas(STEAM)), 255);
    }
}
