//! Edit commands, their results and chunk deltas (SAD §8.1, §8.3).
//!
//! [`World::apply`](crate::world::World::apply) is the only way cells change
//! on the host. It returns one [`ChunkDelta`] per chunk it changed, sorted by
//! [`ChunkPos`], so a replica can reproduce the edit exactly.

use serde::{Deserialize, Serialize};

use crate::chunk::ChunkStorage;
use crate::coords::ChunkPos;
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
}

impl EditOp {
    /// Checks the op against the material table before any cell is touched.
    pub(crate) fn validate(self, materials: &MaterialTable) -> Result<(), RejectReason> {
        match self {
            EditOp::Set(id) | EditOp::Paint(id) if materials.get(id).is_none() => {
                Err(RejectReason::UnknownMaterial(id))
            }
            EditOp::Set(_) | EditOp::Paint(_) | EditOp::Remove | EditOp::Damage(_) => Ok(()),
        }
    }

    /// The material a cell holding `old` becomes, or `None` if the op skips it.
    /// Only meaningful for ops that passed [`EditOp::validate`]. `Damage`
    /// returns `None`: whether it breaks a cell depends on the cell's
    /// accumulated damage, which [`World::apply`](crate::world::World::apply)
    /// tracks.
    pub(crate) fn target(self, old: Option<&MaterialDef>) -> Option<MaterialId> {
        match self {
            EditOp::Set(id) => Some(id),
            EditOp::Remove => old.is_some_and(|d| d.diggable).then_some(MaterialId::AIR),
            EditOp::Paint(id) => old.is_some_and(|d| d.is_solid()).then_some(id),
            EditOp::Damage(_) => None,
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
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RejectReason {
    #[error("material {0:?} is not in the material table")]
    UnknownMaterial(MaterialId),
    #[error("shape has more than {MAX_EDIT_CELLS} cells")]
    TooManyCells,
    #[error("shape lies entirely outside the world bounds")]
    OutsideBounds,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditStatus {
    Applied,
    Rejected(RejectReason),
}

/// What changed in one chunk. `revision` is the chunk's revision after the change.
#[derive(Clone, Debug, PartialEq, Eq)]
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
}

impl ChunkDelta {
    pub fn pos(&self) -> ChunkPos {
        match self {
            ChunkDelta::Cells { pos, .. } | ChunkDelta::Full { pos, .. } => *pos,
        }
    }

    pub fn revision(&self) -> u64 {
        match self {
            ChunkDelta::Cells { revision, .. } | ChunkDelta::Full { revision, .. } => *revision,
        }
    }
}

/// The outcome of [`World::apply`](crate::world::World::apply).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditResult {
    pub command: CommandId,
    pub status: EditStatus,
    /// One per changed chunk, ascending by [`ChunkPos`]. Empty when rejected
    /// or when no cell changed.
    pub deltas: Vec<ChunkDelta>,
    /// Materials produced by broken cells, as (material, count).
    pub yields: Vec<(MaterialId, u32)>,
}

impl EditResult {
    pub(crate) fn rejected(command: CommandId, reason: RejectReason) -> Self {
        Self {
            command,
            status: EditStatus::Rejected(reason),
            deltas: Vec::new(),
            yields: Vec::new(),
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
            ChunkDelta::Full { pos, .. } => panic!("expected Cells delta for {pos:?}, got Full"),
        }
    }

    /// Replays deltas onto a copy of the chunks they name, cell by cell. A
    /// stand-in for `World::apply_delta` (F025) that checks the deltas carry
    /// the whole edit.
    fn replay(before: &World, deltas: &[ChunkDelta]) -> World {
        let mut w = before.clone();
        for d in deltas {
            let pos = d.pos();
            let mut chunk = w.chunk(pos).expect("delta for a resident chunk").clone();
            assert_eq!(d.revision(), chunk.revision + 1, "revision for {pos:?}");
            match d {
                ChunkDelta::Cells { changes, .. } => {
                    for &(i, m) in changes {
                        chunk.storage.set(LocalPos::from_index(i), m);
                    }
                }
                ChunkDelta::Full { storage, .. } => chunk.storage = storage.clone(),
            }
            chunk.revision = d.revision();
            w.insert_chunk(pos, chunk).unwrap();
        }
        w
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
}
