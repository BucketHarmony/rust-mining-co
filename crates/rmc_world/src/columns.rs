//! Per-column `surface_y` cache (SAD-P2 §3 "Columns", §6).
//!
//! `surface_y` of a cell column `(x, z)` is the highest in-bounds resident
//! cell that is `Liquid`, or `Solid` and not `passable`
//! ([`MaterialDef::forms_surface`]); [`NO_SURFACE`] when there is none.
//! "Sky-open" means `y > surface_y`. [`World`](crate::world::World) keeps the
//! cache current in `apply`, `apply_delta` and `insert_chunk`.

use crate::chunk::{Chunk, ChunkStorage};
use crate::coords::{CHUNK_MASK, CHUNK_SHIFT, CHUNK_SIZE, CellAabb, CellPos, ChunkPos, LocalPos};
use crate::fasthash::PosMap;
use crate::materials::{MaterialDef, MaterialId, MaterialTable};

/// `surface_y` of a column with no solid or liquid cell.
pub const NO_SURFACE: i16 = i16::MIN;
/// Cell columns per chunk column.
pub const COLUMN_CELLS: usize = (CHUNK_SIZE * CHUNK_SIZE) as usize;

/// `surface_y` per cell column, stored per chunk column `(cx, cz)` and
/// indexed by `lx + lz * 32`.
#[derive(Clone, Debug, Default)]
pub struct Columns {
    map: PosMap<(i32, i32), [i16; COLUMN_CELLS]>,
}

impl Columns {
    /// `surface_y` of the cell column at `(x, z)`; [`NO_SURFACE`] if no
    /// resident chunk in the column has a surface-forming cell in bounds.
    pub fn surface_y(&self, x: i32, z: i32) -> i16 {
        self.map
            .get(&(x >> CHUNK_SHIFT, z >> CHUNK_SHIFT))
            .map_or(NO_SURFACE, |col| col[index(x, z)])
    }

    /// Recomputes every cell column of chunk column `(cx, cz)` from the
    /// resident chunks. Used when a whole chunk arrives or is replaced.
    pub(crate) fn rebuild(
        &mut self,
        cx: i32,
        cz: i32,
        chunks: &PosMap<ChunkPos, Chunk>,
        materials: &MaterialTable,
        bounds: CellAabb,
    ) {
        let mut col = [NO_SURFACE; COLUMN_CELLS];
        for lz in 0..CHUNK_SIZE {
            for lx in 0..CHUNK_SIZE {
                let (x, z) = ((cx << CHUNK_SHIFT) + lx, (cz << CHUNK_SHIFT) + lz);
                col[index(x, z)] = scan_down(x, z, bounds.max.0.y, chunks, materials, bounds);
            }
        }
        self.map.insert((cx, cz), col);
    }

    /// Updates the column of `cell` after its material changed from `old`
    /// to `new`. `chunks` already holds `new`. Cells outside `bounds` do not
    /// count and are ignored.
    pub(crate) fn cell_changed(
        &mut self,
        cell: CellPos,
        old: MaterialId,
        new: MaterialId,
        chunks: &PosMap<ChunkPos, Chunk>,
        materials: &MaterialTable,
        bounds: CellAabb,
    ) {
        if !bounds.contains(cell) {
            return;
        }
        let (x, y, z) = (cell.0.x, cell.0.y, cell.0.z);
        let y16 = to_i16(y);
        let current = self.surface_y(x, z);
        let next = if forms_surface(materials, new) {
            current.max(y16)
        } else if forms_surface(materials, old) && y16 == current {
            scan_down(x, z, y - 1, chunks, materials, bounds)
        } else {
            current
        };
        if next != current {
            self.map
                .entry((x >> CHUNK_SHIFT, z >> CHUNK_SHIFT))
                .or_insert([NO_SURFACE; COLUMN_CELLS])[index(x, z)] = next;
        }
    }
}

fn index(x: i32, z: i32) -> usize {
    ((x & CHUNK_MASK) + (z & CHUNK_MASK) * CHUNK_SIZE) as usize
}

fn forms_surface(materials: &MaterialTable, id: MaterialId) -> bool {
    materials.get(id).is_some_and(MaterialDef::forms_surface)
}

/// World heights fit `i16` with room for [`NO_SURFACE`]; `World::new`
/// checks the bounds.
fn to_i16(y: i32) -> i16 {
    i16::try_from(y).expect("world y fits i16")
}

/// Highest surface-forming cell of column `(x, z)` at or below `from_y`,
/// inside `bounds`, walking the resident chunks top down.
fn scan_down(
    x: i32,
    z: i32,
    from_y: i32,
    chunks: &PosMap<ChunkPos, Chunk>,
    materials: &MaterialTable,
    bounds: CellAabb,
) -> i16 {
    let (lo, hi) = (bounds.min.0, bounds.max.0);
    if x < lo.x || x > hi.x || z < lo.z || z > hi.z {
        return NO_SURFACE;
    }
    let top = from_y.min(hi.y);
    let bottom = lo.y;
    if top < bottom {
        return NO_SURFACE;
    }
    let (lx, lz) = ((x & CHUNK_MASK) as u32, (z & CHUNK_MASK) as u32);
    for cy in (bottom >> CHUNK_SHIFT..=top >> CHUNK_SHIFT).rev() {
        let pos = ChunkPos::new(x >> CHUNK_SHIFT, cy, z >> CHUNK_SHIFT);
        let Some(chunk) = chunks.get(&pos) else {
            continue;
        };
        let base = cy << CHUNK_SHIFT;
        let (y_hi, y_lo) = (top.min(base + CHUNK_MASK), bottom.max(base));
        if let ChunkStorage::Uniform(id) = chunk.storage {
            if forms_surface(materials, id) {
                return to_i16(y_hi);
            }
            continue;
        }
        for y in (y_lo..=y_hi).rev() {
            let local = LocalPos::new(lx, (y - base) as u32, lz);
            if forms_surface(materials, chunk.storage.get(local)) {
                return to_i16(y);
            }
        }
    }
    NO_SURFACE
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use proptest::prelude::*;

    use super::*;
    use crate::edit::{CommandId, EditCommand, EditOp, PlayerId};
    use crate::shape::Shape;
    use crate::world::World;

    /// Air, a solid, a passable solid, a liquid, and an undiggable solid.
    const TABLE: &str = r#"[
        (id: 0, key: "air", phase: Air, tags: [], hardness: 0, diggable: false,
         yield_count: 0, color: (0, 0, 0, 0), emissive: 0.0),
        (id: 1, key: "rock", phase: Solid, tags: ["stone"], hardness: 1, diggable: true,
         yield_count: 1, color: (1, 1, 1, 255), emissive: 0.0),
        (id: 2, key: "moss", phase: Solid, tags: ["plant"], hardness: 0, diggable: true,
         yield_count: 0, color: (2, 2, 2, 255), emissive: 0.0, passable: true),
        (id: 3, key: "pond", phase: Liquid, tags: [], hardness: 0, diggable: true,
         yield_count: 0, color: (3, 3, 3, 128), emissive: 0.0),
        (id: 4, key: "floor", phase: Solid, tags: [], hardness: 0, diggable: false,
         yield_count: 0, color: (4, 4, 4, 255), emissive: 0.0),
    ]"#;
    const N_MATERIALS: u16 = 5;

    /// Not chunk-aligned on any axis: chunks -1..=0 in x and z, -2..=1 in y.
    fn bounds() -> CellAabb {
        CellAabb::new(CellPos::new(-30, -36, -32), CellPos::new(29, 40, 27))
    }

    /// Columns checked against the brute-force scan: around the chunk
    /// borders, the bounds edges, and one step outside them.
    const XS: [i32; 12] = [-31, -30, -29, -17, -2, -1, 0, 1, 16, 29, 30, 31];
    const ZS: [i32; 12] = [-33, -32, -31, -9, -1, 0, 1, 2, 20, 26, 27, 28];

    fn table() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::from_ron_str(TABLE).expect("test table loads"))
    }

    /// Highest surface-forming cell of the column by scanning every height
    /// through `get_cell`.
    fn brute_force(w: &World, x: i32, z: i32) -> i16 {
        let b = w.bounds();
        (b.min.0.y - 1..=b.max.0.y + 1)
            .rev()
            .find(|&y| {
                w.get_cell(CellPos::new(x, y, z))
                    .and_then(|id| w.materials().get(id))
                    .is_some_and(MaterialDef::forms_surface)
            })
            .map_or(NO_SURFACE, |y| y as i16)
    }

    fn check(w: &World, what: &str) -> Result<(), TestCaseError> {
        for x in XS {
            for z in ZS {
                prop_assert_eq!(
                    w.surface_y(x, z),
                    brute_force(w, x, z),
                    "{} column ({}, {})",
                    what,
                    x,
                    z
                );
            }
        }
        Ok(())
    }

    #[derive(Clone, Debug)]
    enum Op {
        /// 0 Set, 1 Remove, 2 Paint, 3 Damage; then material, box min, box size.
        Edit(u8, u16, (i32, i32, i32), (i32, i32, i32)),
        /// Replace chunk `bounds().chunks()[i]` on both worlds: a uniform
        /// fill with cells written over it in world coordinates.
        Insert(usize, u16, Vec<(i32, i32, i32, u16)>),
    }

    fn cells() -> impl Strategy<Value = Vec<(i32, i32, i32, u16)>> {
        proptest::collection::vec((-34..34i32, -66..66i32, -34..34i32, 0..N_MATERIALS), 0..120)
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (
                0..4u8,
                0..N_MATERIALS,
                (-34..32i32, -40..42i32, -34..30i32),
                (1..24i32, 1..40i32, 1..24i32),
            )
                .prop_map(|(k, m, min, size)| Op::Edit(k, m, min, size)),
            1 => (0..16usize, 0..N_MATERIALS, cells())
                .prop_map(|(i, m, c)| Op::Insert(i, m, c)),
        ]
    }

    fn build_chunk(pos: ChunkPos, fill: u16, cells: &[(i32, i32, i32, u16)]) -> Chunk {
        let mut chunk = Chunk::uniform(MaterialId(fill));
        for &(x, y, z, m) in cells {
            let (p, local) = CellPos::new(x, y, z).split();
            if p == pos {
                chunk.storage.set(local, MaterialId(m));
            }
        }
        chunk
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(
            std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(256)
        ))]

        /// The cache equals a full scan after inserts and after every edit,
        /// on the host (`apply`) and on a replica fed the deltas
        /// (`apply_delta`, both `Cells` and `Full`).
        #[test]
        fn surface_y_matches_brute_force_proptest(
            initial in proptest::collection::vec((0..=N_MATERIALS, cells()), 16),
            ops in proptest::collection::vec(op(), 1..8),
        ) {
            let table = table();
            let all: Vec<ChunkPos> = bounds().chunks().collect();
            prop_assert_eq!(all.len(), 16);
            let mut host = World::new(table.clone(), bounds());
            let mut replica = World::new(table, bounds());
            // Fill N_MATERIALS leaves the chunk out.
            for (&pos, (fill, cells)) in all.iter().zip(&initial) {
                if *fill < N_MATERIALS {
                    let chunk = build_chunk(pos, *fill, cells);
                    host.insert_chunk(pos, chunk.clone()).unwrap();
                    replica.insert_chunk(pos, chunk).unwrap();
                }
            }
            check(&host, "host after insert")?;
            check(&replica, "replica after insert")?;

            for (seq, op) in ops.iter().enumerate() {
                match op {
                    Op::Edit(kind, m, min, size) => {
                        let id = MaterialId(*m);
                        let op = match kind {
                            0 => EditOp::Set(id),
                            1 => EditOp::Remove,
                            2 => EditOp::Paint(id),
                            _ => EditOp::Damage(u16::MAX),
                        };
                        let issuer = PlayerId(1);
                        let cmd = EditCommand {
                            id: CommandId { issuer, seq: seq as u32 },
                            issuer,
                            op,
                            shape: Shape::Box {
                                min: CellPos::new(min.0, min.1, min.2),
                                max: CellPos::new(
                                    min.0 + size.0 - 1,
                                    min.1 + size.1 - 1,
                                    min.2 + size.2 - 1,
                                ),
                            },
                        };
                        let result = host.apply(&cmd);
                        for delta in &result.deltas {
                            replica.apply_delta(delta).unwrap();
                        }
                    }
                    Op::Insert(i, fill, cells) => {
                        let pos = all[*i];
                        let chunk = build_chunk(pos, *fill, cells);
                        host.insert_chunk(pos, chunk.clone()).unwrap();
                        replica.insert_chunk(pos, chunk).unwrap();
                    }
                }
                check(&host, &format!("host after {op:?}"))?;
                check(&replica, &format!("replica after {op:?}"))?;
            }
            prop_assert_eq!(host.hash(), replica.hash());
        }
    }

    #[test]
    fn passable_solids_and_liquids_and_gaps() {
        let table = table();
        let mut w = World::new(table, bounds());
        assert_eq!(w.surface_y(0, 0), NO_SURFACE);
        // Rock in the lower chunk, nothing resident above: top of the rock
        // chunk, clipped to nothing (the chunk at y -1 is below 0).
        w.insert_chunk(ChunkPos::new(0, -2, 0), Chunk::uniform(MaterialId(1)))
            .unwrap();
        assert_eq!(w.surface_y(0, 0), -33);
        assert_eq!(w.surface_y(5, 27), -33);
        // Outside the bounds in z: not counted.
        assert_eq!(w.surface_y(5, 28), NO_SURFACE);
        // Moss above does not form a surface; a pond does.
        w.insert_chunk(ChunkPos::new(0, 0, 0), Chunk::uniform(MaterialId(2)))
            .unwrap();
        assert_eq!(w.surface_y(0, 0), -33);
        w.insert_chunk(ChunkPos::new(0, 1, 0), Chunk::uniform(MaterialId(3)))
            .unwrap();
        // Clipped to bounds.max.y.
        assert_eq!(w.surface_y(0, 0), 40);
        // Neighboring chunk column untouched.
        assert_eq!(w.surface_y(-1, 0), NO_SURFACE);
    }
}
