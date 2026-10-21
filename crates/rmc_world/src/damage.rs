//! Cell damage (SAD §7, §8.1): pickaxe hits accumulate per cell until the
//! cell's hardness is reached, then it breaks.
//!
//! Damage lives on the host [`World`](crate::world::World) only. It is not
//! hashed, saved or sent to replicas, and it resets on load. `Set`, `Paint`
//! and `Remove` on a cell clear its damage.

use std::collections::BTreeMap;

use crate::coords::{CellPos, ChunkPos};
use crate::layers::LayerId;
use crate::materials::MaterialDef;
use crate::world::World;

/// Divisor of the hardness of a cell softened by regolith (SAD-P2 §7.5).
const SOFT_DIV: u16 = 2;

impl World {
    /// Damage that breaks `cell`, which holds `def`: its hardness, or
    /// `hardness / 2` when its regolith is above the displacement rules'
    /// `soft_regolith` (SAD-P2 §7.5). Without that constant (no eco tables)
    /// regolith never softens a cell.
    pub fn break_hardness(&self, cell: CellPos, def: &MaterialDef) -> u16 {
        let soft = self
            .rules
            .soft_regolith
            .is_some_and(|s| self.layer(cell, LayerId::Regolith) > i32::from(s));
        if soft {
            def.hardness / SOFT_DIV
        } else {
            def.hardness
        }
    }
}

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
    use crate::displace::EcoRules;
    use crate::edit::{
        ChunkDelta, CommandId, EditCommand, EditOp, EditResult, EditStatus, PlayerId, RejectReason,
    };
    use crate::materials::{MaterialId, MaterialTable};
    use crate::shape::Shape;

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
        // The row's yield_material when it has one (SAD-P2 §7.5).
        let yielded = def.yield_material.as_deref().map_or(m, |k| id(table, k));
        let expected_yield = if def.yield_count == 0 {
            vec![]
        } else {
            vec![(yielded, u32::from(def.yield_count))]
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

        // Every diggable solid material yields itself (or its
        // yield_material, SAD-P2 §7.5), yield_count times.
        let yielded = |m: MaterialId| {
            let def = table.get(m).unwrap();
            def.yield_material.as_deref().map_or(m, |k| id(&table, k))
        };
        for def in table.iter().filter(|d| d.diggable && d.is_solid()) {
            let mut w = world(table.clone(), &def.key);
            let r = hit(&mut w, 1, cell(1, 2, 3), u16::MAX);
            assert_eq!(r.deltas.len(), 1, "{}", def.key);
            let expected = if def.yield_count == 0 {
                vec![]
            } else {
                vec![(yielded(def.id), u32::from(def.yield_count))]
            };
            assert_eq!(r.yields, expected, "{}", def.key);
        }
        let rich = id(&table, "rich");
        let richest = id(&table, "richest");
        let barren = id(&table, "barren");

        // A breaking area aggregates per material, ascending by id, and
        // leaves out zero yields. Layout along x: stone, rich, barren,
        // richest, sulfide_ore (one column each, 2 × 2 cells), over the
        // chunk border.
        let stone = id(&table, "stone");
        let copper = id(&table, "sulfide_ore");
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
            (yielded(copper), 4 * copper_yield),
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

    /// `eco.soft_regolith` in assets/eco.ron.
    const SOFT: u8 = 128;

    /// [`world`] with the displacement rules' `soft_regolith` set to `soft`.
    fn eco_world(table: Arc<MaterialTable>, fill: &str, soft: Option<u8>) -> World {
        let mut w = world(table, fill);
        w.set_eco_rules(EcoRules {
            soft_regolith: soft,
            ..EcoRules::default()
        });
        w
    }

    /// Sets the regolith layer of one cell through the edit path.
    fn set_regolith(w: &mut World, seq: u32, at: CellPos, value: u8) {
        let op = EditOp::SetLayer {
            layer: LayerId::Regolith,
            value: i32::from(value),
        };
        apply(w, &cmd(seq, op, Shape::Cell(at)));
        assert_eq!(w.layer(at, LayerId::Regolith), i32::from(value));
    }

    /// Hits `at` with `dmg` until it breaks and returns the number of hits.
    /// Every hit before the last leaves the cell standing.
    fn hits_to_break(w: &mut World, at: CellPos, dmg: u16) -> u32 {
        let m = w.get_cell(at).unwrap();
        for k in 1..=1000 {
            let r = hit(w, 1000 + k, at, dmg);
            if w.get_cell(at) == Some(MaterialId::AIR) {
                assert!(!r.deltas.is_empty());
                return k;
            }
            assert_eq!(w.get_cell(at), Some(m));
            assert!(r.yields.is_empty());
        }
        panic!("{at:?} never broke");
    }

    #[test]
    fn regolith_halves_hardness() {
        let table = table_with(&[("odd", 45, 1), ("feather", 1, 1), ("instant", 0, 1)]);
        let at = cell(31, 7, 12);

        // For every diggable solid and several damages, a cell needs
        // ⌈hardness / dmg⌉ hits at regolith ≤ soft_regolith and
        // ⌈(hardness / 2) / dmg⌉ above it (at least one hit).
        let diggable: Vec<MaterialDef> = table
            .iter()
            .filter(|d| d.diggable && d.is_solid())
            .cloned()
            .collect();
        for def in &diggable {
            for dmg in [1, 2, 7, 15, 29, u16::MAX] {
                for (regolith, need) in [
                    (0, def.hardness),
                    (SOFT - 1, def.hardness),
                    (SOFT, def.hardness),
                    (SOFT + 1, def.hardness / 2),
                    (u8::MAX, def.hardness / 2),
                ] {
                    let mut w = eco_world(table.clone(), &def.key, Some(SOFT));
                    set_regolith(&mut w, 1, at, regolith);
                    let expected = ceil_div(need, dmg).max(1);
                    assert_eq!(
                        hits_to_break(&mut w, at, dmg),
                        expected,
                        "{} hardness {} regolith {regolith} dmg {dmg}",
                        def.key,
                        def.hardness
                    );
                }
            }
        }

        // Hand-computed: stone 30 → 15, odd 45 → 22 (truncating), feather
        // 1 → 0, which breaks on a zero-damage hit.
        let stone = table.get(id(&table, "stone")).unwrap().hardness;
        assert_eq!(stone, 30);
        for (key, dmg, soft_hits, hard_hits) in [
            ("stone", 15, 1, 2),
            ("stone", 14, 2, 3),
            ("odd", 22, 1, 3),
            ("odd", 21, 2, 3),
        ] {
            let mut w = eco_world(table.clone(), key, Some(SOFT));
            set_regolith(&mut w, 1, at, SOFT + 1);
            assert_eq!(hits_to_break(&mut w, at, dmg), soft_hits, "{key} {dmg}");
            let mut w = eco_world(table.clone(), key, Some(SOFT));
            assert_eq!(hits_to_break(&mut w, at, dmg), hard_hits, "{key} {dmg}");
        }
        let mut w = eco_world(table.clone(), "feather", Some(SOFT));
        set_regolith(&mut w, 1, at, u8::MAX);
        let r = hit(&mut w, 2, at, 0);
        assert_eq!(r.yields, vec![(id(&table, "feather"), 1)]);
        assert_eq!(w.get_cell(at), Some(MaterialId::AIR));

        // Only the cell's own regolith counts: soft neighbors do not soften it.
        let mut w = eco_world(table.clone(), "stone", Some(SOFT));
        for (seq, n) in (1..).zip([cell(30, 7, 12), cell(32, 7, 12), cell(31, 8, 12)]) {
            set_regolith(&mut w, seq, n, u8::MAX);
        }
        assert_eq!(hits_to_break(&mut w, at, 15), 2);

        // Damage dealt before the cell softened counts toward the halved
        // hardness: 10 on hard stone, then 5 once soft breaks it.
        let mut w = eco_world(table.clone(), "stone", Some(SOFT));
        let before = w.clone();
        let r = hit(&mut w, 1, at, 10);
        assert_no_change(&r, &before, &w);
        assert_eq!(w.damage().get(at), 10);
        set_regolith(&mut w, 2, at, 200);
        let r = hit(&mut w, 3, at, 5);
        assert_eq!(r.yields, vec![(id(&table, "stone"), 1)]);
        assert_eq!(w.get_cell(at), Some(MaterialId::AIR));
        assert!(w.damage().is_empty());

        // An area hit across the chunk border: soft cells break, hard ones
        // only take damage.
        let mut w = eco_world(table.clone(), "stone", Some(SOFT));
        let soft = [cell(30, 0, 0), cell(33, 0, 0)];
        for (seq, &c) in (1..).zip(&soft) {
            set_regolith(&mut w, seq, c, SOFT + 1);
        }
        let r = apply(
            &mut w,
            &cmd(9, EditOp::Damage(15), boxed(cell(30, 0, 0), cell(33, 0, 0))),
        );
        assert_eq!(r.yields, vec![(id(&table, "stone"), 2)]);
        for x in 30..=33 {
            let broken = soft.contains(&cell(x, 0, 0));
            assert_eq!(w.get_cell(cell(x, 0, 0)) == Some(MaterialId::AIR), broken);
            assert_eq!(w.damage().get(cell(x, 0, 0)), if broken { 0 } else { 15 });
        }

        // Without eco rules (Phase 1 worlds) regolith does not soften.
        let mut w = eco_world(table.clone(), "stone", None);
        set_regolith(&mut w, 1, at, u8::MAX);
        assert_eq!(hits_to_break(&mut w, at, 15), 2);
        let mut w = world(table.clone(), "stone");
        set_regolith(&mut w, 1, at, u8::MAX);
        assert_eq!(hits_to_break(&mut w, at, 15), 2);

        // Undiggable cells stay unbreakable however soft.
        let mut w = eco_world(table.clone(), "bedrock", Some(SOFT));
        set_regolith(&mut w, 1, at, u8::MAX);
        let before = w.clone();
        let r = hit(&mut w, 2, at, u16::MAX);
        assert_no_change(&r, &before, &w);
        assert!(w.damage().is_empty());

        // The shipped soft_regolith is the one these cases use.
        let eco =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/eco.ron"))
                .expect("assets/eco.ron reads");
        assert!(eco.contains(&format!("soft_regolith: {SOFT},")));
    }

    /// The Phase 1 table plus: `crushed` (stone copy, yield_count 2),
    /// `ore` (yields `crushed`, yield_count 3), `dud_ore` (yields `crushed`,
    /// yield_count 0) and `self_ore` (yields itself by `yield_material`).
    fn yield_table() -> Arc<MaterialTable> {
        let base = phase1();
        let stone = base.get(id(&base, "stone")).unwrap().clone();
        let mut defs: Vec<MaterialDef> = base.iter().cloned().collect();
        for (key, yield_count, yield_material) in [
            ("crushed", 2, None),
            ("ore", 3, Some("crushed")),
            ("dud_ore", 0, Some("crushed")),
            ("self_ore", 4, Some("self_ore")),
        ] {
            let n = u16::try_from(defs.len()).unwrap();
            defs.push(MaterialDef {
                id: MaterialId(n),
                key: key.to_string(),
                yield_count,
                yield_material: yield_material.map(str::to_string),
                ..stone.clone()
            });
        }
        Arc::new(MaterialTable::from_defs(defs).expect("test table is valid"))
    }

    #[test]
    fn yield_material_overrides_self() {
        let table = yield_table();
        let crushed = id(&table, "crushed");
        let ore = id(&table, "ore");
        let self_ore = id(&table, "self_ore");
        let stone = id(&table, "stone");

        // Every diggable solid yields its yield_material (or itself when it
        // has none), its own yield_count times.
        for def in table.iter().filter(|d| d.diggable && d.is_solid()) {
            let mut w = world(table.clone(), &def.key);
            let r = hit(&mut w, 1, cell(1, 2, 3), u16::MAX);
            assert_eq!(r.deltas.len(), 1, "{}", def.key);
            let to = def
                .yield_material
                .as_deref()
                .map_or(def.id, |k| id(&table, k));
            let expected = if def.yield_count == 0 {
                vec![]
            } else {
                vec![(to, u32::from(def.yield_count))]
            };
            assert_eq!(r.yields, expected, "{}", def.key);
        }

        // Hand-written: ore yields 3 crushed and no ore; dud_ore yields
        // nothing; self_ore yields itself.
        let mut w = world(table.clone(), "ore");
        let r = hit(&mut w, 1, cell(0, 0, 0), u16::MAX);
        assert_eq!(r.yields, vec![(crushed, 3)]);
        let mut w = world(table.clone(), "dud_ore");
        let r = hit(&mut w, 1, cell(0, 0, 0), u16::MAX);
        assert_eq!(r.deltas.len(), 1);
        assert!(r.yields.is_empty());
        let mut w = world(table.clone(), "self_ore");
        let r = hit(&mut w, 1, cell(0, 0, 0), u16::MAX);
        assert_eq!(r.yields, vec![(self_ore, 4)]);

        // An area over the chunk border: yields aggregate under the yielded
        // material. Columns along x (2 × 2 cells each): ore, crushed, stone,
        // dud_ore, ore.
        let mut w = world(table.clone(), "stone");
        let columns = ["ore", "crushed", "stone", "dud_ore", "ore"];
        for (i, key) in columns.iter().enumerate() {
            let x = 30 + i32::try_from(i).unwrap();
            let op = EditOp::Set(id(&table, key));
            apply(&mut w, &cmd(1, op, boxed(cell(x, 0, 0), cell(x, 1, 1))));
        }
        let r = apply(
            &mut w,
            &cmd(
                2,
                EditOp::Damage(u16::MAX),
                boxed(cell(30, 0, 0), cell(34, 1, 1)),
            ),
        );
        let stone_yield = u32::from(table.get(stone).unwrap().yield_count);
        let mut expected = vec![(stone, 4 * stone_yield), (crushed, 2 * 4 * 3 + 4 * 2)];
        expected.sort_unstable_by_key(|&(m, _)| m);
        assert_eq!(r.yields, expected);
        assert!(!r.yields.iter().any(|&(m, _)| m == ore));

        // No yield without a break, and none from Remove.
        let mut w = world(table.clone(), "ore");
        let r = hit(&mut w, 1, cell(0, 0, 0), 29);
        assert!(r.yields.is_empty());
        let r = apply(&mut w, &cmd(2, EditOp::Remove, Shape::Cell(cell(0, 0, 0))));
        assert_eq!(r.deltas.len(), 1);
        assert!(r.yields.is_empty(), "Remove has no yield");

        // A soft ore cell breaks at half hardness and still yields crushed.
        let mut w = eco_world(table.clone(), "ore", Some(SOFT));
        set_regolith(&mut w, 1, cell(0, 0, 0), u8::MAX);
        let r = hit(&mut w, 2, cell(0, 0, 0), 15);
        assert_eq!(r.yields, vec![(crushed, 3)]);
    }

    #[test]
    fn sulfide_yields_crushed_sulfide() {
        let table = phase1();
        let ore = id(&table, "sulfide_ore");
        let crushed = id(&table, "crushed_sulfide");
        let ore_def = table.get(ore).unwrap().clone();
        let crushed_def = table.get(crushed).unwrap().clone();
        let pairs = |v: &[(&str, u8)]| -> Vec<(String, u8)> {
            v.iter().map(|&(k, a)| (k.to_string(), a)).collect()
        };

        // The shipped rows (SAD-P2 §5). sulfide_ore keeps its Phase 1 tag
        // `ore`, gains `sulfide`, and yields crushed_sulfide.
        assert!(ore_def.is_solid() && ore_def.diggable);
        assert!(ore_def.has_tag("ore") && ore_def.has_tag("sulfide"));
        assert_eq!(ore_def.yield_material.as_deref(), Some("crushed_sulfide"));
        assert!(ore_def.yield_count > 0);
        let sulfide = pairs(&[("cu", 60), ("fe", 40), ("s", 80)]);
        assert_eq!(ore_def.composition, sulfide);
        // crushed_sulfide: placeable porous tailings with the ore's
        // composition, yielding itself.
        assert!(crushed_def.is_solid() && crushed_def.diggable);
        for tag in ["sulfide", "porous", "placeable"] {
            assert!(crushed_def.has_tag(tag), "crushed_sulfide lacks {tag}");
        }
        assert!(!crushed_def.has_tag("ore"));
        assert_eq!(
            (
                crushed_def.permeability,
                crushed_def.field_capacity,
                crushed_def.weathering_rate
            ),
            (96, 48, 48)
        );
        assert_eq!(crushed_def.composition, sulfide);
        assert_eq!(crushed_def.yield_material, None);
        assert!(crushed_def.yield_count > 0);

        // Breaking one sulfide cell takes ⌈hardness / dmg⌉ hits and yields
        // crushed_sulfide, never sulfide_ore.
        let n = u32::from(ore_def.yield_count);
        let at = cell(31, 7, 12);
        for dmg in [1, 7, ore_def.hardness, u16::MAX] {
            let mut w = world(table.clone(), "sulfide_ore");
            let hits = hits_to_break(&mut w, at, dmg);
            assert_eq!(hits, ceil_div(ore_def.hardness, dmg), "dmg {dmg}");
        }
        let mut w = world(table.clone(), "sulfide_ore");
        let r = hit(&mut w, 1, at, u16::MAX);
        assert_eq!(r.deltas.len(), 1);
        assert_eq!(r.yields, vec![(crushed, n)]);
        assert_eq!(w.get_cell(at), Some(MaterialId::AIR));

        // A soft (weathered) sulfide cell breaks at half hardness and still
        // yields crushed_sulfide.
        let mut w = eco_world(table.clone(), "sulfide_ore", Some(SOFT));
        set_regolith(&mut w, 1, at, u8::MAX);
        let r = hit(&mut w, 2, at, ore_def.hardness / 2);
        assert_eq!(r.yields, vec![(crushed, n)]);

        // An area over the chunk border (columns along x, 2 × 2 cells
        // each): sulfide, crushed, stone, sulfide. Sulfide and crushed
        // aggregate under crushed_sulfide; nothing yields sulfide_ore.
        let stone = id(&table, "stone");
        let mut w = world(table.clone(), "stone");
        for (i, m) in [ore, crushed, stone, ore].into_iter().enumerate() {
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
                boxed(cell(30, 0, 0), cell(33, 1, 1)),
            ),
        );
        let stone_yield = u32::from(table.get(stone).unwrap().yield_count);
        let crushed_yield = u32::from(crushed_def.yield_count);
        let mut expected = vec![
            (stone, 4 * stone_yield),
            (crushed, 2 * 4 * n + 4 * crushed_yield),
        ];
        expected.sort_unstable_by_key(|&(m, _)| m);
        assert_eq!(r.yields, expected);
        assert!(!r.yields.iter().any(|&(m, _)| m == ore));
    }
}
