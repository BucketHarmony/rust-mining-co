//! Cell damage (SAD §7, §8.1): pickaxe hits accumulate per cell until the
//! cell's hardness is reached, then it breaks.
//!
//! Damage lives on the host [`World`](crate::world::World) only. It is not
//! hashed, saved or sent to replicas, and it resets on load. `Set`, `Paint`
//! and `Remove` on a cell clear its damage.

use std::collections::BTreeMap;

use crate::coords::{CellPos, ChunkPos};

/// Accumulated damage per cell, in [`CellPos`] order. A cell with no entry
/// has no damage. Every entry is below its cell's hardness and above zero.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DamageMap(BTreeMap<CellPos, u16>);

impl DamageMap {
    /// Damage accumulated on `cell` (0 when undamaged).
    pub fn get(&self, cell: CellPos) -> u16 {
        self.0.get(&cell).copied().unwrap_or(0)
    }

    /// Number of damaged cells.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Damaged cells and their damage, ascending by [`CellPos`].
    pub fn iter(&self) -> impl Iterator<Item = (CellPos, u16)> + '_ {
        self.0.iter().map(|(c, d)| (*c, *d))
    }

    /// Adds `dmg` to `cell` (saturating) and reports whether it broke, i.e.
    /// whether its total reached `hardness`. A broken cell's entry is removed.
    /// The caller has already checked that the cell is diggable.
    pub(crate) fn hit(&mut self, cell: CellPos, hardness: u16, dmg: u16) -> bool {
        let total = self.get(cell).saturating_add(dmg);
        if total >= hardness {
            self.0.remove(&cell);
            true
        } else {
            if total > 0 {
                self.0.insert(cell, total);
            }
            false
        }
    }

    /// Forgets the damage on `cell`.
    pub(crate) fn clear(&mut self, cell: CellPos) {
        self.0.remove(&cell);
    }

    /// Forgets the damage on every cell of chunk `pos`.
    pub(crate) fn clear_chunk(&mut self, pos: ChunkPos) {
        self.0.retain(|cell, _| cell.chunk() != pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::chunk::Chunk;
    use crate::coords::CellAabb;
    use crate::edit::{
        ChunkDelta, CommandId, EditCommand, EditOp, EditResult, EditStatus, PlayerId, RejectReason,
    };
    use crate::materials::{MaterialDef, MaterialId, MaterialTable};
    use crate::shape::Shape;
    use crate::world::World;

    const PHASE1_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    fn phase1() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(PHASE1_PATH).expect("assets/materials.ron loads"))
    }

    /// The Phase 1 table plus extra test materials, each a copy of stone with
    /// the given key, hardness and yield count, appended in order.
    fn table_with(extra: &[(&str, u16, u8)]) -> Arc<MaterialTable> {
        let base = phase1();
        let stone = base.get(id(&base, "stone")).unwrap().clone();
        let mut defs: Vec<MaterialDef> = base.iter().cloned().collect();
        for &(key, hardness, yield_count) in extra {
            let n = u16::try_from(defs.len()).unwrap();
            defs.push(MaterialDef {
                id: MaterialId(n),
                key: key.to_string(),
                hardness,
                yield_count,
                ..stone.clone()
            });
        }
        Arc::new(MaterialTable::from_defs(defs).expect("test table is valid"))
    }

    fn id(table: &MaterialTable, key: &str) -> MaterialId {
        table
            .id(key)
            .unwrap_or_else(|| panic!("material {key} in table"))
    }

    /// A 2 × 1 × 1 chunk world (cells 0..=63 × 0..=31 × 0..=31), every chunk
    /// resident and filled with `fill`.
    fn world(table: Arc<MaterialTable>, fill: &str) -> World {
        let fill = id(&table, fill);
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 31));
        let mut w = World::new(table, bounds);
        for pos in bounds.chunks() {
            w.insert_chunk(pos, Chunk::uniform(fill)).unwrap();
        }
        w
    }

    fn cmd(seq: u32, op: EditOp, shape: Shape) -> EditCommand {
        let issuer = PlayerId(3);
        EditCommand {
            id: CommandId { issuer, seq },
            issuer,
            op,
            shape,
        }
    }

    fn cell(x: i32, y: i32, z: i32) -> CellPos {
        CellPos::new(x, y, z)
    }

    fn boxed(min: CellPos, max: CellPos) -> Shape {
        Shape::Box { min, max }
    }

    /// Applies `c` and checks it was applied.
    fn apply(w: &mut World, c: &EditCommand) -> EditResult {
        let r = w.apply(c);
        assert_eq!(r.command, c.id);
        assert_eq!(r.status, EditStatus::Applied, "{c:?}");
        r
    }

    /// Hits `cell` once with `dmg`.
    fn hit(w: &mut World, seq: u32, at: CellPos, dmg: u16) -> EditResult {
        apply(w, &cmd(seq, EditOp::Damage(dmg), Shape::Cell(at)))
    }

    /// Sets one cell to `m` through the edit path (it also clears damage).
    fn set(w: &mut World, seq: u32, at: CellPos, m: MaterialId) {
        apply(w, &cmd(seq, EditOp::Set(m), Shape::Cell(at)));
    }

    fn revisions(w: &World) -> Vec<(ChunkPos, u64, bool)> {
        w.iter_chunks_sorted()
            .map(|(p, c)| (p, c.revision, c.modified))
            .collect()
    }

    /// Asserts a damage command changed nothing visible: no deltas, no
    /// yields, same cells, revisions and `modified` flags.
    fn assert_no_change(r: &EditResult, before: &World, after: &World) {
        assert!(r.deltas.is_empty(), "unexpected deltas {:?}", r.deltas);
        assert!(r.yields.is_empty(), "unexpected yields {:?}", r.yields);
        assert_eq!(after.hash(), before.hash());
        assert_eq!(revisions(after), revisions(before));
    }

    /// Hits needed to break a cell of hardness `h` with `dmg` per hit.
    fn ceil_div(h: u16, dmg: u16) -> u32 {
        u32::from(h).div_ceil(u32::from(dmg))
    }

    /// Hits one cell until it breaks and checks it takes exactly
    /// ⌈hardness / dmg⌉ hits: every earlier hit only accumulates damage, the
    /// breaking hit turns the cell to air with one delta and the yield.
    fn assert_breaks_after_ceil(table: &Arc<MaterialTable>, key: &str, dmg: u16) {
        let m = id(table, key);
        let def = table.get(m).unwrap().clone();
        let mut w = world(table.clone(), key);
        let at = cell(31, 7, 12);
        let (pos, local) = at.split();
        let hits = if def.hardness == 0 {
            1
        } else {
            ceil_div(def.hardness, dmg)
        };

        for k in 1..hits {
            let before = w.clone();
            let r = hit(&mut w, k, at, dmg);
            assert_no_change(&r, &before, &w);
            assert_eq!(w.get_cell(at), Some(m), "{key} dmg {dmg} hit {k}");
            assert_eq!(
                u32::from(w.damage().get(at)),
                k * u32::from(dmg),
                "{key} dmg {dmg} hit {k}"
            );
            assert_eq!(w.damage().len(), 1);
        }

        let r = hit(&mut w, hits, at, dmg);
        assert_eq!(
            r.deltas,
            vec![ChunkDelta::Cells {
                pos,
                revision: 1,
                changes: vec![(local.index(), MaterialId::AIR)],
            }],
            "{key} dmg {dmg}: breaks on hit {hits}"
        );
        let expected_yield = if def.yield_count == 0 {
            vec![]
        } else {
            vec![(m, u32::from(def.yield_count))]
        };
        assert_eq!(r.yields, expected_yield, "{key} dmg {dmg}");
        assert_eq!(w.get_cell(at), Some(MaterialId::AIR));
        assert!(w.chunk(pos).unwrap().modified);
        assert!(w.damage().is_empty(), "broken cell keeps no damage");

        // Hitting the air left behind does nothing.
        let before = w.clone();
        let r = hit(&mut w, hits + 1, at, dmg);
        assert_no_change(&r, &before, &w);
        assert!(w.damage().is_empty());
    }

    #[test]
    fn breaks_on_ceil_hardness_over_dmg() {
        let table = table_with(&[("instant", 0, 1), ("feather", 1, 1)]);
        let diggable: Vec<String> = table
            .iter()
            .filter(|d| d.diggable && d.is_solid())
            .map(|d| d.key.clone())
            .collect();
        assert!(diggable.len() >= 10, "table has the Phase 1 diggables");

        // Divisors, non-divisors, exactly the hardness, one either side of
        // it, and far above it.
        for key in &diggable {
            let h = table.get(id(&table, key)).unwrap().hardness;
            let mut dmgs = vec![1, 2, 3, 7, 10, 29, 100, u16::MAX];
            dmgs.extend([h.saturating_sub(1), h, h.saturating_add(1)]);
            for dmg in dmgs.into_iter().filter(|&d| d > 0) {
                assert_breaks_after_ceil(&table, key, dmg);
            }
        }

        // Spot checks against hand-computed counts: stone (30), granite (45).
        assert_eq!(ceil_div(30, 7), 5);
        assert_eq!(ceil_div(45, 45), 1);
        assert_eq!(ceil_div(45, 44), 2);

        // Hardness 0 breaks on the first hit, even with zero damage.
        let mut w = world(table.clone(), "instant");
        let at = cell(0, 0, 0);
        let r = hit(&mut w, 1, at, 0);
        assert_eq!(r.deltas.len(), 1);
        assert_eq!(r.yields, vec![(id(&table, "instant"), 1)]);
        assert_eq!(w.get_cell(at), Some(MaterialId::AIR));

        // Zero damage on a hard cell accumulates nothing and stores nothing.
        let mut w = world(table.clone(), "stone");
        let before = w.clone();
        let r = hit(&mut w, 1, at, 0);
        assert_no_change(&r, &before, &w);
        assert!(w.damage().is_empty());

        // A box of mixed hardness: stone (30) and granite (45) hit with 15
        // per command. Stone breaks on hit 2, granite on hit 3; each break
        // is one delta per chunk, across the x = 31 | 32 border.
        let mut w = world(table.clone(), "stone");
        let stone = id(&table, "stone");
        let granite = id(&table, "granite");
        let (min, max) = (cell(30, 0, 0), cell(33, 1, 0));
        apply(
            &mut w,
            &cmd(
                1,
                EditOp::Set(granite),
                boxed(cell(30, 1, 0), cell(33, 1, 0)),
            ),
        );
        let area = cmd(2, EditOp::Damage(15), boxed(min, max));
        let before = w.clone();
        let r = apply(&mut w, &area);
        assert_no_change(&r, &before, &w);
        assert_eq!(w.damage().len(), 8);
        assert!(w.damage().iter().all(|(_, d)| d == 15));

        let r = apply(&mut w, &area);
        assert_eq!(r.deltas.len(), 2, "stone row breaks in both chunks");
        assert_eq!(r.yields, vec![(stone, 4)]);
        for x in 30..=33 {
            assert_eq!(w.get_cell(cell(x, 0, 0)), Some(MaterialId::AIR));
            assert_eq!(w.get_cell(cell(x, 1, 0)), Some(granite));
            assert_eq!(w.damage().get(cell(x, 1, 0)), 30);
        }
        assert_eq!(w.damage().len(), 4);

        let r = apply(&mut w, &area);
        assert_eq!(r.deltas.len(), 2);
        assert_eq!(r.yields, vec![(granite, 4)]);
        assert!(w.damage().is_empty());
        for x in 30..=33 {
            for y in 0..=1 {
                assert_eq!(w.get_cell(cell(x, y, 0)), Some(MaterialId::AIR));
            }
        }
    }

    #[test]
    fn bedrock_never_breaks() {
        let table = phase1();
        let bedrock = id(&table, "bedrock");
        assert!(!table.get(bedrock).unwrap().diggable);

        // Single cell, the largest damage, many times.
        let mut w = world(table.clone(), "bedrock");
        let at = cell(31, 31, 31);
        let before = w.clone();
        for seq in 0..100 {
            let r = hit(&mut w, seq, at, u16::MAX);
            assert_no_change(&r, &before, &w);
        }
        assert_eq!(w.get_cell(at), Some(bedrock));
        assert!(w.damage().is_empty(), "undiggable cells gather no damage");

        // A whole chunk of bedrock: applied, nothing changes.
        let all = cmd(
            100,
            EditOp::Damage(u16::MAX),
            boxed(cell(0, 0, 0), cell(31, 31, 31)),
        );
        let r = apply(&mut w, &all);
        assert_no_change(&r, &before, &w);
        assert!(w.damage().is_empty());
        assert!(revisions(&w).iter().all(|&(_, rev, m)| rev == 0 && !m));

        // Mixed: a bedrock floor under stone. The stone breaks, the floor stays.
        let mut w = world(table.clone(), "stone");
        let stone = id(&table, "stone");
        apply(
            &mut w,
            &cmd(
                1,
                EditOp::Set(bedrock),
                boxed(cell(0, 0, 0), cell(63, 0, 31)),
            ),
        );
        let r = apply(
            &mut w,
            &cmd(
                2,
                EditOp::Damage(u16::MAX),
                boxed(cell(10, 0, 10), cell(40, 3, 12)),
            ),
        );
        assert_eq!(r.yields, vec![(stone, 31 * 3 * 3)]);
        for x in 10..=40 {
            for z in 10..=12 {
                assert_eq!(w.get_cell(cell(x, 0, z)), Some(bedrock));
                for y in 1..=3 {
                    assert_eq!(w.get_cell(cell(x, y, z)), Some(MaterialId::AIR));
                }
            }
        }
        assert!(w.damage().is_empty());

        // Remove skips bedrock too (F021), and a hit afterwards still does
        // nothing.
        let before = w.clone();
        apply(&mut w, &cmd(3, EditOp::Remove, Shape::Cell(cell(5, 0, 5))));
        let r = hit(&mut w, 4, cell(5, 0, 5), u16::MAX);
        assert_no_change(&r, &before, &w);
        assert_eq!(w.get_cell(cell(5, 0, 5)), Some(bedrock));
    }

    #[test]
    fn yields_match_table() {
        let table = table_with(&[("barren", 30, 0), ("rich", 30, 3), ("richest", 30, 255)]);

        // Every diggable solid material yields itself, yield_count times.
        for def in table.iter().filter(|d| d.diggable && d.is_solid()) {
            let mut w = world(table.clone(), &def.key);
            let r = hit(&mut w, 1, cell(1, 2, 3), u16::MAX);
            assert_eq!(r.deltas.len(), 1, "{}", def.key);
            let expected = if def.yield_count == 0 {
                vec![]
            } else {
                vec![(def.id, u32::from(def.yield_count))]
            };
            assert_eq!(r.yields, expected, "{}", def.key);
        }
        let rich = id(&table, "rich");
        let richest = id(&table, "richest");
        let barren = id(&table, "barren");

        // A breaking area aggregates per material, ascending by id, and
        // leaves out zero yields. Layout along x: stone, rich, barren,
        // richest, copper_ore (one column each, 2 × 2 cells), over the
        // chunk border.
        let stone = id(&table, "stone");
        let copper = id(&table, "copper_ore");
        let mut w = world(table.clone(), "stone");
        let columns = [stone, rich, barren, richest, copper];
        for (i, &m) in columns.iter().enumerate() {
            let x = 30 + i32::try_from(i).unwrap();
            apply(
                &mut w,
                &cmd(1, EditOp::Set(m), boxed(cell(x, 0, 0), cell(x, 1, 1))),
            );
        }
        let r = apply(
            &mut w,
            &cmd(
                2,
                EditOp::Damage(u16::MAX),
                boxed(cell(30, 0, 0), cell(34, 1, 1)),
            ),
        );
        let copper_yield = u32::from(table.get(copper).unwrap().yield_count);
        let stone_yield = u32::from(table.get(stone).unwrap().yield_count);
        let mut expected = vec![
            (stone, 4 * stone_yield),
            (copper, 4 * copper_yield),
            (rich, 12),
            (richest, 4 * 255),
        ];
        expected.sort_unstable_by_key(|&(m, _)| m);
        assert_eq!(r.yields, expected);
        assert!(!r.yields.iter().any(|&(m, _)| m == barren));
        assert!(r.yields.windows(2).all(|p| p[0].0 < p[1].0));

        // No yield without a break, and none from Remove or Set.
        let mut w = world(table.clone(), "rich");
        let r = hit(&mut w, 1, cell(0, 0, 0), 29);
        assert!(r.yields.is_empty());
        let r = apply(&mut w, &cmd(2, EditOp::Remove, Shape::Cell(cell(0, 0, 0))));
        assert_eq!(r.deltas.len(), 1);
        assert!(r.yields.is_empty(), "Remove has no yield");
        let r = apply(
            &mut w,
            &cmd(3, EditOp::Set(MaterialId::AIR), Shape::Cell(cell(1, 0, 0))),
        );
        assert!(r.yields.is_empty(), "Set has no yield");

        // A rejected damage command yields nothing.
        let r = w.apply(&cmd(
            4,
            EditOp::Damage(u16::MAX),
            boxed(cell(0, 0, 0), cell(63, 31, 31)),
        ));
        assert_eq!(r.status, EditStatus::Rejected(RejectReason::TooManyCells));
        assert!(r.yields.is_empty() && r.deltas.is_empty());
        assert_eq!(w.get_cell(cell(5, 5, 5)), Some(rich));
    }

    #[test]
    fn saturates() {
        let table = table_with(&[("adamant", u16::MAX, 1)]);
        let adamant = id(&table, "adamant");
        let at = cell(40, 20, 10);

        // 40,000 + 40,000 overflows u16; saturating gives 65,535, which
        // reaches the hardness and breaks.
        let mut w = world(table.clone(), "adamant");
        let before = w.clone();
        let r = hit(&mut w, 1, at, 40_000);
        assert_no_change(&r, &before, &w);
        assert_eq!(w.damage().get(at), 40_000);
        let r = hit(&mut w, 2, at, 40_000);
        assert_eq!(r.deltas.len(), 1);
        assert_eq!(r.yields, vec![(adamant, 1)]);
        assert_eq!(w.get_cell(at), Some(MaterialId::AIR));
        assert!(w.damage().is_empty());

        // One short of the maximum, then one more.
        let mut w = world(table.clone(), "adamant");
        hit(&mut w, 1, at, u16::MAX - 1);
        assert_eq!(w.damage().get(at), u16::MAX - 1);
        hit(&mut w, 2, at, 0);
        assert_eq!(w.damage().get(at), u16::MAX - 1, "zero damage adds nothing");
        let r = hit(&mut w, 3, at, 1);
        assert_eq!(r.deltas.len(), 1);
        assert_eq!(w.get_cell(at), Some(MaterialId::AIR));

        // u16::MAX in one hit breaks the hardest diggable cell.
        let mut w = world(table.clone(), "adamant");
        let r = hit(&mut w, 1, at, u16::MAX);
        assert_eq!(r.deltas.len(), 1);

        // Repeated maximal hits on a softer, already damaged cell saturate
        // rather than wrap: stone breaks on the first u16::MAX hit after any
        // prior damage.
        let mut w = world(table.clone(), "stone");
        hit(&mut w, 1, at, 29);
        let r = hit(&mut w, 2, at, u16::MAX);
        assert_eq!(r.deltas.len(), 1);
        assert!(w.damage().is_empty());

        // DamageMap::hit directly: saturating total, entry kept below the
        // hardness, removed on break.
        let mut d = DamageMap::default();
        assert!(!d.hit(at, u16::MAX, u16::MAX - 5));
        assert!(!d.hit(at, u16::MAX, 4));
        assert_eq!(d.get(at), u16::MAX - 1);
        assert!(d.hit(at, u16::MAX, u16::MAX));
        assert_eq!(d.get(at), 0);
        assert!(d.is_empty());
        assert!(!d.hit(at, 10, 0));
        assert!(d.is_empty(), "zero damage stores no entry");
        assert!(d.hit(at, 0, 0), "hardness 0 is instant");
    }

    #[test]
    fn set_clears_damage() {
        let table = phase1();
        let stone = id(&table, "stone");
        let granite = id(&table, "granite");
        let at = cell(31, 4, 4);
        let neighbor = cell(32, 4, 4);

        // Set to a different material: the new cell starts undamaged and
        // takes the full ⌈45 / 10⌉ = 5 hits.
        let mut w = world(table.clone(), "stone");
        hit(&mut w, 1, at, 20);
        hit(&mut w, 2, neighbor, 20);
        assert_eq!(w.damage().get(at), 20);
        set(&mut w, 3, at, granite);
        assert_eq!(w.damage().get(at), 0, "Set clears damage");
        assert_eq!(w.damage().get(neighbor), 20, "other cells keep theirs");
        for seq in 4..8 {
            let r = hit(&mut w, seq, at, 10);
            assert!(r.deltas.is_empty(), "granite not broken by hit {seq}");
        }
        let r = hit(&mut w, 8, at, 10);
        assert_eq!(r.yields, vec![(granite, 1)]);

        // Set to the same material changes no cell but still clears damage:
        // stone then needs all ⌈30 / 10⌉ = 3 hits again.
        let mut w = world(table.clone(), "stone");
        hit(&mut w, 1, at, 20);
        let before = w.clone();
        let r = apply(&mut w, &cmd(2, EditOp::Set(stone), Shape::Cell(at)));
        assert!(r.deltas.is_empty());
        assert_eq!(revisions(&w), revisions(&before));
        assert_eq!(w.damage().get(at), 0);
        assert!(hit(&mut w, 3, at, 10).deltas.is_empty());
        assert!(hit(&mut w, 4, at, 10).deltas.is_empty());
        assert_eq!(hit(&mut w, 5, at, 10).deltas.len(), 1);

        // Paint and Remove clear it as well.
        let mut w = world(table.clone(), "stone");
        hit(&mut w, 1, at, 20);
        apply(&mut w, &cmd(2, EditOp::Paint(granite), Shape::Cell(at)));
        assert_eq!(w.get_cell(at), Some(granite));
        assert_eq!(w.damage().get(at), 0, "Paint clears damage");

        let mut w = world(table.clone(), "stone");
        hit(&mut w, 1, at, 20);
        apply(&mut w, &cmd(2, EditOp::Remove, Shape::Cell(at)));
        assert_eq!(w.damage().get(at), 0, "Remove clears damage");
        set(&mut w, 3, at, stone);
        assert!(hit(&mut w, 4, at, 20).deltas.is_empty(), "fresh stone");
        assert_eq!(w.damage().get(at), 20);

        // An area Set clears every damaged cell inside it and none outside.
        let mut w = world(table.clone(), "stone");
        let damaged = [
            cell(30, 0, 0),
            cell(31, 1, 1),
            cell(32, 2, 2),
            cell(40, 0, 0),
        ];
        for (seq, &c) in (1..).zip(&damaged) {
            hit(&mut w, seq, c, 5);
        }
        assert_eq!(w.damage().len(), 4);
        apply(
            &mut w,
            &cmd(
                9,
                EditOp::Set(granite),
                boxed(cell(30, 0, 0), cell(33, 2, 2)),
            ),
        );
        assert_eq!(
            w.damage().iter().collect::<Vec<_>>(),
            vec![(cell(40, 0, 0), 5)]
        );

        // A rejected Set clears nothing.
        let r = w.apply(&cmd(
            10,
            EditOp::Set(stone),
            boxed(cell(0, 0, 0), cell(63, 31, 31)),
        ));
        assert_eq!(r.status, EditStatus::Rejected(RejectReason::TooManyCells));
        assert_eq!(w.damage().get(cell(40, 0, 0)), 5);

        // Replacing a chunk with insert_chunk drops the damage inside it.
        hit(&mut w, 11, cell(0, 0, 0), 5);
        assert_eq!(w.damage().len(), 2);
        w.insert_chunk(ChunkPos::new(1, 0, 0), Chunk::uniform(stone))
            .unwrap();
        assert_eq!(
            w.damage().iter().collect::<Vec<_>>(),
            vec![(cell(0, 0, 0), 5)]
        );
    }
}
