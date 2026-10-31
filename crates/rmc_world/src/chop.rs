//! Trees as geometry, interaction (docs/TREES.md T5): which limbs are
//! solid, chopping them, felling, and what a cut or felled tree leaves.
//!
//! A limb is solid when its pipe radius is at least
//! [`TreeRules::solid_radius_um`]. Its solid volume is the box around its
//! internode (both ends, grown by the radius on every axis), in integer
//! millimeters of the world: the player collides with these boxes, the
//! tool ray hits them and a placed cell may not overlap one. Twigs and
//! leaves are passable.
//!
//! `EditOp::Chop { metamer, dmg }` on a tree's root cell adds `dmg` to the
//! limb's chop damage. The limb breaks at its wood's hardness times its
//! radius in whole `solid_radius_um` (at least once), so thick limbs take
//! more hits. A broken limb takes its subtree: the wood yields whole items
//! of its species' wood by volume (`wood_per_item_dm3`), and every leaf on
//! it lays `metamer_litter` organic on the first anchor below its end,
//! ledgered `litter_in`. Chopping the first internode fells the tree; the
//! root cell stays wood and may sprout again.
//!
//! When the root cell's plant goes (dug, burnt, replaced), the standing
//! graph falls whole: a dig yields its wood as items, any other cause lays
//! `metamer_litter` per metamer as litter instead; leaves are litter either
//! way.
//!
//! Chop damage, like cell damage, lives on the host only and is forgotten
//! whenever the tree's graph changes.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::coords::{CELL_SIZE_MM, CellAabb, CellPos, ChunkPos, LocalPos};
use crate::displace::{Cause, Effects};
use crate::layers::LayerId;
use crate::ledger::SeasonLedger;
use crate::tree::{NO_PARENT, TreeGraph};
use crate::world::World;

/// Chunks around a region searched for trees whose limbs reach into it.
pub const TREE_REACH_CHUNKS: i32 = 1;
/// π as 355/113, for wood volume.
const PI_NUM: u128 = 355;
const PI_DEN: u128 = 113;
/// µm² per mm².
const UM2_PER_MM2: u128 = 1_000_000;
/// mm³ per dm³.
const MM3_PER_DM3: u128 = 1_000_000;
/// µm per mm.
const UM_PER_MM: u32 = 1_000;

/// What the world needs to know about graph trees (`plants.ron` and
/// `eco.ron`). The default has no graph species: no limb is solid and
/// nothing drops.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeRules {
    /// Twig radius per plant species index; 0 for species that do not grow
    /// as graphs.
    pub tip_um: Vec<u32>,
    /// Ring per cycle of age per plant species index
    /// ([`TreeGraph::wood_radii`]); missing means 0.
    pub ring_um: Vec<u32>,
    /// `eco.solid_radius_um`: from this radius a limb is solid. 0: none is.
    pub solid_radius_um: u32,
    /// `eco.metamer_litter`: organic per fallen leaf (and per metamer of a
    /// tree that falls without being dug).
    pub metamer_litter: u8,
    /// `eco.wood_per_item_dm3`: wood volume per item. 0: wood yields none.
    pub wood_per_item_dm3: u32,
}

/// The solid volume of one limb, in world millimeters, `min <= max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeBox {
    pub root: CellPos,
    pub metamer: u16,
    pub min_mm: [i32; 3],
    pub max_mm: [i32; 3],
}

impl TreeBox {
    /// Whether the box shares volume with `cell` (touching faces do not).
    pub fn overlaps_cell(&self, cell: CellPos) -> bool {
        let lo = cell.0.to_array().map(|c| c * CELL_SIZE_MM);
        (0..3).all(|a| self.min_mm[a] < lo[a] + CELL_SIZE_MM && self.max_mm[a] > lo[a])
    }
}

/// Where a tree's metamer coordinates start: its root cell's top-face
/// center, in world millimeters.
pub fn root_origin_mm(root: CellPos) -> [i32; 3] {
    let half = CELL_SIZE_MM / 2;
    [
        root.0.x * CELL_SIZE_MM + half,
        (root.0.y + 1) * CELL_SIZE_MM,
        root.0.z * CELL_SIZE_MM + half,
    ]
}

/// The cell holding the point `at` millimeters from `root`'s origin.
pub fn cell_at(root: CellPos, at: [i32; 3]) -> CellPos {
    let o = root_origin_mm(root);
    let [x, y, z] = [0, 1, 2].map(|a| (o[a] + at[a]).div_euclid(CELL_SIZE_MM));
    CellPos::new(x, y, z)
}

/// Wood volume of the marked metamers in mm³: π r² L each.
fn volume_mm3(graph: &TreeGraph, radii: &[u32], marked: &[bool]) -> u128 {
    let sum: u128 = graph
        .metamers()
        .iter()
        .zip(radii)
        .zip(marked)
        .filter(|(_, m)| **m)
        .map(|((m, &r), _)| u128::from(r) * u128::from(r) * u128::from(m.length_mm))
        .sum();
    sum * PI_NUM / PI_DEN / UM2_PER_MM2
}

impl World {
    /// The tree rules in force.
    pub fn tree_rules(&self) -> &TreeRules {
        &self.tree_rules
    }

    /// Sets the tree rules. The host and every replica set the same rules
    /// (the replica only reads the solid limbs).
    pub fn set_tree_rules(&mut self, rules: TreeRules) {
        self.tree_rules = Arc::new(rules);
    }

    /// The yearly-ring growth of the graph tree rooted at `root` (per cycle
    /// of age, [`TreeGraph::wood_radii`]); 0 without a plant or a ring.
    pub fn tree_ring_um(&self, root: CellPos) -> u32 {
        self.plant(root)
            .and_then(|p| self.tree_rules.ring_um.get(usize::from(p.species)))
            .copied()
            .unwrap_or(0)
    }

    /// Twig radius of the graph tree rooted at `root`, from its plant's
    /// species; `None` without a plant or for a species not grown as a
    /// graph.
    pub fn tree_tip_um(&self, root: CellPos) -> Option<u32> {
        let species = self.plant(root)?.species;
        self.tree_rules
            .tip_um
            .get(usize::from(species))
            .copied()
            .filter(|&t| t > 0)
    }

    /// The solid limbs of the tree rooted at `root`, in metamer order.
    pub fn limb_boxes(&self, root: CellPos) -> Vec<TreeBox> {
        let solid = self.tree_rules.solid_radius_um;
        let (Some(graph), Some(tip)) = (self.tree(root), self.tree_tip_um(root)) else {
            return Vec::new();
        };
        if solid == 0 {
            return Vec::new();
        }
        let o = root_origin_mm(root);
        let ends = graph.ends();
        let radii = graph.wood_radii(tip, self.tree_ring_um(root));
        let mut out = Vec::new();
        for (i, m) in graph.metamers().iter().enumerate() {
            if radii[i] < solid {
                continue;
            }
            let start = if m.parent == NO_PARENT {
                [0; 3]
            } else {
                ends[usize::from(m.parent)]
            };
            let r = i32::try_from(radii[i].div_ceil(UM_PER_MM)).unwrap_or(i32::MAX);
            let end = ends[i];
            out.push(TreeBox {
                root,
                metamer: i as u16,
                min_mm: [0, 1, 2].map(|a| o[a] + start[a].min(end[a]) - r),
                max_mm: [0, 1, 2].map(|a| o[a] + start[a].max(end[a]) + r),
            });
        }
        out
    }

    /// Every solid limb box overlapping `region`, from the trees rooted in
    /// the chunks within [`TREE_REACH_CHUNKS`] of it: chunks in (y, z, x)
    /// order, roots ascending, metamers ascending.
    pub fn tree_boxes(&self, region: CellAabb) -> Vec<TreeBox> {
        if self.tree_rules.solid_radius_um == 0 {
            return Vec::new();
        }
        let lo = region.min.chunk().0 - TREE_REACH_CHUNKS;
        let hi = region.max.chunk().0 + TREE_REACH_CHUNKS;
        let rmin = region.min.0.to_array().map(|c| c * CELL_SIZE_MM);
        let rmax = region.max.0.to_array().map(|c| (c + 1) * CELL_SIZE_MM);
        let mut out = Vec::new();
        for y in lo.y..=hi.y {
            for z in lo.z..=hi.z {
                for x in lo.x..=hi.x {
                    let pos = ChunkPos::new(x, y, z);
                    let Some(chunk) = self.chunks.get(&pos) else {
                        continue;
                    };
                    for &index in chunk.trees.keys() {
                        let root = CellPos::from_chunk_local(pos, LocalPos::from_index(index));
                        out.extend(self.limb_boxes(root).into_iter().filter(|b| {
                            (0..3).all(|a| b.min_mm[a] < rmax[a] && b.max_mm[a] > rmin[a])
                        }));
                    }
                }
            }
        }
        out
    }

    /// Whether `cell` overlaps a solid limb.
    pub fn in_tree(&self, cell: CellPos) -> bool {
        self.tree_boxes(CellAabb::new(cell, cell))
            .iter()
            .any(|b| b.overlaps_cell(cell))
    }

    /// Whether metamer `metamer` of the tree rooted at `root` is a solid
    /// limb.
    pub fn is_solid_limb(&self, root: CellPos, metamer: u16) -> bool {
        self.limb_boxes(root).iter().any(|b| b.metamer == metamer)
    }

    /// Chop damage on a limb (0 when undamaged).
    pub fn chop_damage(&self, root: CellPos, metamer: u16) -> u16 {
        self.chop_damage.get(&(root, metamer)).copied().unwrap_or(0)
    }

    /// The `Chop` edit on one limb, already validated.
    pub(crate) fn chop(
        &mut self,
        root: CellPos,
        metamer: u16,
        dmg: u16,
        cause: Cause,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) {
        let (Some(graph), Some(tip)) = (self.tree(root).cloned(), self.tree_tip_um(root)) else {
            return;
        };
        let radii = graph.wood_radii(tip, self.tree_ring_um(root));
        let i = usize::from(metamer);
        let hardness = self
            .plant(root)
            .and_then(|p| self.rules.plant_woods.get(usize::from(p.species)).copied())
            .flatten()
            .and_then(|w| self.materials.get(w))
            .map_or(1, |d| d.hardness.max(1));
        let times = (radii[i] / self.tree_rules.solid_radius_um.max(1)).max(1);
        let hardness = hardness.saturating_mul(u16::try_from(times).unwrap_or(u16::MAX));
        let total = self.chop_damage(root, metamer).saturating_add(dmg);
        if total < hardness {
            self.chop_damage.insert((root, metamer), total);
            return;
        }
        let mut cut = vec![false; graph.len()];
        cut[i] = true;
        let (kept, gone) = graph.prune(&cut);
        self.drop_metamers(root, &graph, &radii, &gone, cause, ledger, fx);
        self.put_tree(root, (!kept.is_empty()).then_some(kept), fx);
    }

    /// The graph rooted at `cell` falls whole: its root cell changed
    /// material, and its plant is about to go.
    pub(crate) fn fell(
        &mut self,
        cell: CellPos,
        cause: Cause,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) {
        let Some(graph) = self.tree(cell).cloned() else {
            return;
        };
        let tip = self.tree_tip_um(cell).unwrap_or(0);
        let radii = graph.wood_radii(tip, self.tree_ring_um(cell));
        let all = vec![true; graph.len()];
        self.drop_metamers(cell, &graph, &radii, &all, cause, ledger, fx);
        self.put_tree(cell, None, fx);
    }

    /// What the marked metamers of the tree at `root` leave: wood items by
    /// volume when `cause` harvests (else `metamer_litter` per metamer as
    /// litter), and `metamer_litter` per leaf as litter.
    #[allow(clippy::too_many_arguments)]
    fn drop_metamers(
        &mut self,
        root: CellPos,
        graph: &TreeGraph,
        radii: &[u32],
        marked: &[bool],
        cause: Cause,
        ledger: &mut SeasonLedger,
        fx: &mut Effects,
    ) {
        let rules = Arc::clone(&self.tree_rules);
        let items = cause.harvests() || cause.chops();
        if items && rules.wood_per_item_dm3 > 0 {
            let wood = self
                .plant(root)
                .and_then(|p| self.rules.plant_woods.get(usize::from(p.species)).copied())
                .flatten();
            let per = u128::from(rules.wood_per_item_dm3) * MM3_PER_DM3;
            let n = u32::try_from(volume_mm3(graph, radii, marked) / per).unwrap_or(u32::MAX);
            if let Some(wood) = wood
                && n > 0
            {
                *fx.yields.entry(wood).or_insert(0) += n;
            }
        }
        let per = i32::from(rules.metamer_litter);
        let ends = graph.ends();
        let mut litter: BTreeMap<CellPos, i32> = BTreeMap::new();
        for ((m, end), _) in graph
            .metamers()
            .iter()
            .zip(&ends)
            .zip(marked)
            .filter(|(_, m)| **m)
        {
            let amount = per * i32::from(m.leaf) + if items { 0 } else { per };
            if amount == 0 {
                continue;
            }
            if let Some(landing) = self.litter_landing(cell_at(root, *end)) {
                *litter.entry(landing).or_insert(0) += amount;
            }
        }
        let Some(c) = self.rules.organic else {
            return;
        };
        for (landing, amount) in litter {
            let added = self.deposit(landing, LayerId::Substance(c), amount, fx);
            ledger.litter_in[usize::from(c)] += i64::from(added);
        }
    }

    /// The first solid cell below `from` that is no plant's part or plant,
    /// if it is an anchor material.
    fn litter_landing(&self, from: CellPos) -> Option<CellPos> {
        let floor = self.bounds.min.0.y;
        let top = from.0.y.min(self.bounds.max.0.y + 1);
        let landing = (floor..top)
            .rev()
            .map(|y| CellPos::new(from.0.x, y, from.0.z))
            .find(|&c| {
                self.is_solid(c) && self.layer(c, LayerId::Part) == 0 && self.plant(c).is_none()
            })?;
        let anchor = self
            .get_cell(landing)
            .and_then(|id| self.materials.get(id))
            .is_some_and(|d| d.anchor);
        anchor.then_some(landing)
    }

    /// Replaces the graph rooted at `root` by an edit, recording it; the
    /// light is dirty and the tree's chop damage is forgotten.
    fn put_tree(&mut self, root: CellPos, tree: Option<TreeGraph>, fx: &mut Effects) {
        let (pos, local) = root.split();
        let Some(chunk) = self.chunks.get_mut(&pos) else {
            return;
        };
        match &tree {
            Some(t) => chunk.trees.insert(local.index(), t.clone()),
            None => chunk.trees.remove(&local.index()),
        };
        fx.trees.entry(pos).or_default().insert(local.index(), tree);
        fx.touched.insert(root);
        self.light_dirty = true;
        self.chop_damage.retain(|&(r, _), _| r != root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EcoRules;
    use crate::chunk::Chunk;
    use crate::edit::{
        ChunkDelta, CommandId, EditCommand, EditOp, EditResult, EditStatus, PlayerId, RejectReason,
    };
    use crate::layers::PlantState;
    use crate::materials::{MaterialId, MaterialTable};
    use crate::raycast::raycast_tree;
    use crate::shape::Shape;
    use crate::tree::{BudState, Dir, Metamer, Q14};
    use glam::Vec3;

    const MATERIALS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const UP: Dir = [0, Q14 as i16, 0];
    const EAST: Dir = [Q14 as i16, 0, 0];
    const ORGANIC: u8 = 0;
    /// A 20 cm twig: every limb is solid at 8 cm.
    const THICK_TIP: u32 = 200_000;
    /// A 6 cm twig: only the trunk under the fork (√2 · 6 = 8.5 cm) is.
    const THIN_TIP: u32 = 60_000;
    const SOLID: u32 = 80_000;
    const WOOD_DM3: u32 = 10;

    fn table() -> Arc<MaterialTable> {
        Arc::new(MaterialTable::load(MATERIALS).unwrap())
    }

    fn id(t: &MaterialTable, key: &str) -> MaterialId {
        t.id(key).unwrap()
    }

    fn m(parent: u16, dir: Dir, leaf: u8, lateral: bool) -> Metamer {
        Metamer {
            parent,
            dir,
            length_mm: 500,
            born: 0,
            bud: BudState::Active,
            leaf,
            shade: 0,
            lateral,
        }
    }

    /// A 2 m trunk (metamers 0–3) with a 1 m limb going east from the top
    /// of the second metamer (4–5); leaves on the tips.
    fn tree() -> TreeGraph {
        TreeGraph::try_from(vec![
            m(NO_PARENT, UP, 0, false),
            m(0, UP, 0, false),
            m(1, UP, 0, false),
            m(2, UP, 2, false),
            m(1, EAST, 0, true),
            m(4, EAST, 3, false),
        ])
        .unwrap()
    }

    /// Two chunks of air over a loam floor (y 0..=1), an oak root at
    /// (8, 2, 8) with `tree()`.
    fn world(tip_um: u32) -> (World, CellPos) {
        let t = table();
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 31));
        let mut w = World::new(Arc::clone(&t), bounds);
        let oak = id(&t, "oak_wood");
        w.set_eco_rules(EcoRules {
            organic: Some(ORGANIC),
            plant_woods: vec![Some(oak)],
            ..EcoRules::default()
        });
        w.set_tree_rules(TreeRules {
            tip_um: vec![tip_um],
            ring_um: Vec::new(),
            solid_radius_um: SOLID,
            metamer_litter: 1,
            wood_per_item_dm3: WOOD_DM3,
        });
        for x in 0..2 {
            w.insert_chunk(ChunkPos::new(x, 0, 0), Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        let loam = id(&t, "loam");
        w.apply(&cmd(
            1,
            EditOp::Set(loam),
            Shape::Box {
                min: CellPos::new(0, 0, 0),
                max: CellPos::new(63, 1, 31),
            },
        ));
        let root = CellPos::new(8, 2, 8);
        w.apply(&cmd(2, EditOp::Set(oak), Shape::Cell(root)));
        let (pos, local) = root.split();
        let chunk = w.chunks.get_mut(&pos).unwrap();
        chunk.plants.insert(local.index(), PlantState::default());
        chunk.trees.insert(local.index(), tree());
        (w, root)
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

    fn chop(w: &mut World, seq: u32, root: CellPos, metamer: u16, dmg: u16) -> EditResult {
        w.apply(&cmd(seq, EditOp::Chop { metamer, dmg }, Shape::Cell(root)))
    }

    fn off(c: CellPos, x: i32, y: i32, z: i32) -> CellPos {
        CellPos::new(c.0.x + x, c.0.y + y, c.0.z + z)
    }

    fn organic(w: &World, cell: CellPos) -> i32 {
        w.layer(cell, LayerId::Substance(ORGANIC))
    }

    #[test]
    fn solid_limbs_are_boxes_around_their_internodes() {
        let (w, root) = world(THICK_TIP);
        let boxes = w.limb_boxes(root);
        assert_eq!(boxes.len(), 6, "every limb is thick");
        let o = root_origin_mm(root);
        // The trunk base: 0..500 mm up, grown by its radius √2 · 20 cm (the
        // pipe model under a two-tip fork), rounded up to the millimeter.
        let r = 283;
        assert_eq!(boxes[0].min_mm, [o[0] - r, o[1] - r, o[2] - r]);
        assert_eq!(boxes[0].max_mm, [o[0] + r, o[1] + 500 + r, o[2] + r]);
        // The east limb ends 1 m east of the trunk at 1 m up.
        assert_eq!(boxes[5].max_mm[0], o[0] + 1000 + 200);
        let (w, root) = world(THIN_TIP);
        let thin: Vec<u16> = w.limb_boxes(root).iter().map(|b| b.metamer).collect();
        assert_eq!(thin, vec![0, 1]);
        assert!(
            w.in_tree(off(root, 0, 1, 0)),
            "the trunk fills the cell above the root"
        );
        assert!(
            !w.in_tree(CellPos::new(9, 5, 8)),
            "the thin east limb is passable"
        );
        assert!(!w.in_tree(off(root, 0, 5, 0)), "so is the thin top");
    }

    #[test]
    fn chopping_a_limb_drops_wood_by_volume_and_leaves_as_litter() {
        let (mut w, root) = world(THICK_TIP);
        let oak = id(w.materials(), "oak_wood");
        let hardness = w.materials().get(oak).unwrap().hardness;
        // The east limb's first metamer is 20 cm across: two solid radii.
        let first = chop(&mut w, 3, root, 4, 2 * hardness - 1);
        assert_eq!(first.status, EditStatus::Applied);
        assert!(first.deltas.is_empty() && first.yields.is_empty());
        assert_eq!(w.chop_damage(root, 4), 2 * hardness - 1);
        let soil_below_tip = CellPos::new(10, 1, 8);
        let before = organic(&w, soil_below_tip);
        let r = chop(&mut w, 4, root, 4, 1);
        // π (r4² + r5²) L = π · 2 · 0.04 m² · 0.5 m = 125.7 dm³: 12 items.
        assert_eq!(r.yields, vec![(oak, 12)]);
        assert_eq!(w.tree(root).unwrap().len(), 4, "the limb went with its tip");
        assert_eq!(
            organic(&w, soil_below_tip),
            before + 3,
            "three leaves on the tip"
        );
        assert_eq!(r.ledger.litter_in[usize::from(ORGANIC)], 3);
        assert_eq!(w.chop_damage(root, 4), 0);
        // The delta carries the new graph.
        let trees: Vec<_> = r
            .deltas
            .iter()
            .filter_map(|d| match d {
                ChunkDelta::Layers { trees, .. } => Some(trees.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(trees, vec![(root.local().index(), w.tree(root).cloned())]);
    }

    #[test]
    fn thick_limbs_take_more_hits() {
        let (mut w, root) = world(THICK_TIP);
        let oak = id(w.materials(), "oak_wood");
        let hardness = w.materials().get(oak).unwrap().hardness;
        // Metamer 5 (20 cm) breaks at twice the hardness, metamer 1
        // (28 cm) at three times.
        chop(&mut w, 3, root, 5, 2 * hardness);
        assert_eq!(w.tree(root).unwrap().len(), 5);
        chop(&mut w, 4, root, 1, 2 * hardness);
        assert_eq!(w.tree(root).unwrap().len(), 5, "the trunk holds");
        chop(&mut w, 5, root, 1, hardness);
        assert_eq!(w.tree(root).unwrap().len(), 1, "only the base is left");
    }

    #[test]
    fn chopping_the_first_internode_fells_the_tree() {
        let (mut w, root) = world(THICK_TIP);
        let oak = id(w.materials(), "oak_wood");
        let r = chop(&mut w, 3, root, 0, u16::MAX);
        assert!(w.tree(root).is_none());
        assert!(w.plant(root).is_some(), "the stump lives on");
        // π (2 · 2 + 4) · 0.04 m² · 0.5 m = 502.7 dm³.
        assert_eq!(r.yields, vec![(oak, 50)]);
        assert_eq!(r.ledger.litter_in[usize::from(ORGANIC)], 5);
    }

    #[test]
    fn digging_the_root_fells_the_standing_graph() {
        let (mut w, root) = world(THICK_TIP);
        let r = w.apply(&cmd(3, EditOp::Damage(u16::MAX), Shape::Cell(root)));
        assert_eq!(w.get_cell(root), Some(MaterialId::AIR));
        assert!(w.tree(root).is_none());
        let oak = id(w.materials(), "oak_wood");
        assert_eq!(
            r.yields,
            vec![(oak, 1 + 50)],
            "the root cell and the tree's wood"
        );
        assert_eq!(r.ledger.litter_in[usize::from(ORGANIC)], 5);
        // Removed by the editor brush instead: no items, the wood rots.
        let (mut w, root) = world(THICK_TIP);
        let r = w.apply(&cmd(3, EditOp::Remove, Shape::Cell(root)));
        assert!(w.tree(root).is_none() && r.yields.is_empty());
        assert_eq!(r.ledger.litter_in[usize::from(ORGANIC)], 5 + 6);
    }

    #[test]
    fn chops_need_a_tree_and_a_solid_limb() {
        let (mut w, root) = world(THIN_TIP);
        let r = chop(&mut w, 3, root, 5, 1);
        assert_eq!(
            r.status,
            EditStatus::Rejected(RejectReason::NoLimb { root, metamer: 5 })
        );
        let r = chop(&mut w, 4, root, 9, 1);
        assert!(matches!(
            r.status,
            EditStatus::Rejected(RejectReason::NoLimb { .. })
        ));
        let r = chop(&mut w, 5, off(root, 1, 0, 0), 0, 1);
        assert!(matches!(
            r.status,
            EditStatus::Rejected(RejectReason::NoLimb { .. })
        ));
        let r = w.apply(&cmd(
            6,
            EditOp::Chop { metamer: 0, dmg: 1 },
            Shape::Box {
                min: root,
                max: root,
            },
        ));
        assert_eq!(r.status, EditStatus::Rejected(RejectReason::ChopNeedsCell));
    }

    #[test]
    fn placing_into_a_trunk_is_refused() {
        let (mut w, root) = world(THICK_TIP);
        let stone = id(w.materials(), "stone");
        let above = off(root, 0, 1, 0);
        let r = w.apply(&cmd(3, EditOp::Place(stone), Shape::Cell(above)));
        assert_eq!(
            r.status,
            EditStatus::Rejected(RejectReason::PlaceInTree(above))
        );
        let clear = off(root, 0, 0, 4);
        let r = w.apply(&cmd(4, EditOp::Place(stone), Shape::Cell(clear)));
        assert_eq!(r.status, EditStatus::Applied);
    }

    #[test]
    fn a_chop_replays_on_a_replica() {
        let (mut host, root) = world(THICK_TIP);
        let (mut replica, _) = world(THICK_TIP);
        let r = chop(&mut host, 3, root, 4, u16::MAX);
        for d in &r.deltas {
            replica.apply_delta(d).unwrap();
        }
        assert_eq!(replica.tree(root), host.tree(root));
        assert_eq!(replica.hash(), host.hash());
        let fell = chop(&mut host, 4, root, 0, u16::MAX);
        for d in &fell.deltas {
            replica.apply_delta(d).unwrap();
        }
        assert!(replica.tree(root).is_none());
        assert_eq!(replica.hash(), host.hash());
    }

    #[test]
    fn the_ray_hits_a_trunk_before_the_cell_behind_it() {
        let (w, root) = world(THICK_TIP);
        let o = root_origin_mm(root).map(|c| c as f32 / 1000.0);
        let eye = Vec3::new(o[0] - 3.0, o[1] + 0.25, o[2]);
        let hit = raycast_tree(&w, eye, Vec3::X, 10.0).expect("the trunk");
        assert_eq!((hit.root, hit.metamer), (root, 0));
        assert!(
            (hit.distance - (3.0 - 0.283)).abs() < 1e-3,
            "{}",
            hit.distance
        );
        assert!(raycast_tree(&w, eye, Vec3::NEG_X, 10.0).is_none());
        assert!(
            raycast_tree(&w, eye, Vec3::X, 2.0).is_none(),
            "out of reach"
        );
    }
}
