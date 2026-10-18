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
            EditOp::Set(_) | EditOp::Paint(_) | EditOp::Remove => Ok(()),
            EditOp::Damage(_) => Err(RejectReason::Unsupported),
        }
    }

    /// The material a cell holding `old` becomes, or `None` if the op skips it.
    /// Only meaningful for ops that passed [`EditOp::validate`].
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
    #[error("this edit op is not supported yet")]
    Unsupported,
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
        // only a few changes in the same command, gets Cells.
        let mut w = two_chunk_world("stone");
        let before = w.clone();
        let r = w.apply(&cmd(2, EditOp::Set(granite), boxed((0, 0, 0), (32, 31, 0))));
        assert_eq!(r.deltas.len(), 2);
        // Chunk 0 changes 32 × 32 = 1024 cells: Cells.
        assert_eq!(cells_delta(&r.deltas[0]).2.len(), 1024);
        let r2 = w.apply(&cmd(
            3,
            EditOp::Set(granite),
            boxed((0, 0, 0), (32, 31, 31)),
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
}
