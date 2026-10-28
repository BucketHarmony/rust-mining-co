//! Multi-cell plants (docs/VEGETATION.md V4): trees grow parts.
//!
//! A multi-cell plant is a `plants.ron` row with `parts`. Its root is the
//! plant cell (the row's material, with a [`PlantState`] and the plant
//! rules of [`crate::plants`]); every other cell of it is a part: a cell of
//! the row's `wood` or `leaf` material whose [`LayerId::Part`] value names
//! its kind and the face neighbor it hangs from. Parent chains end at the
//! root, so the plant is the tree of cells reachable from it.
//!
//! **Biomass.** The growth pass puts a root's growth past 255 into its
//! [`LayerId::Biomass`]; each extension step adds `lit · leaf_feed / 256`,
//! where `lit` counts the leaves whose light is at least `leaf_light`.
//!
//! **Extension (sweep, after spread)**, per root in the global cell order,
//! on the sub-steps whose calendar hour `part_hours` divides (every
//! sub-step of a `seasons.ron` season): feed, then, when the biomass is at
//! least `cost`, grow one part toward the form and pay `cost`. The new part
//! is placed at once (`World::eco_set`, then its part value), so roots
//! later in the order see it. A part grows into an open cell: air, or
//! water (rain stands on wood), which the §7.0 rules push aside.
//!
//! **Tree form.** The trunk grows straight up from the root while its
//! height (root included) is below the tree's seeded height in `trunk`
//! and the cell above is open. Then leaves fill the crown, an ellipsoid of
//! horizontal radius `crown.0` and vertical radius `crown.1` centered on
//! the cell above the trunk's top: the next leaf is the open cell in the
//! crown, face-adjacent to the plant, nearest the center (ties broken by a
//! seeded hash of its offset from the root, so the shape does not depend
//! on where the tree stands). It hangs from its first neighbor in
//! [`PART_DIRS`] order that belongs to the plant.
//!
//! **Severing** is a displacement rule (`rmc_world::displace`): a cell
//! whose material changes stops being a part, and the parts hanging from it
//! become air in turn, each leaving `eco.part_litter` in the anchor below
//! it. Felling the trunk takes the crown; a dead root takes the tree.

use std::collections::{BTreeSet, VecDeque};

use rmc_world::coords::{CellPos, LocalPos};
use rmc_world::layers::{LayerId, PART_DIRS, Part, PartKind, SWEEP_EXTEND};
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::MaterialId;
use rmc_world::world::World;
use xxhash_rust::xxh3::Xxh3;

use crate::runner::{SweepCtx, SweepPass, SweepReport, run_sweep};
use crate::tables::{EcoTables, FormDef};

/// The divisor of `leaf_feed` (256ths of biomass per lit leaf).
const FEED_DIV: i32 = 256;

/// One multi-cell species' part rule, resolved to ids and widened.
#[derive(Clone, Debug)]
struct Form {
    root: MaterialId,
    wood: MaterialId,
    leaf: MaterialId,
    cost: i32,
    leaf_feed: i32,
    leaf_light: i32,
    /// Inclusive trunk height range, root included.
    trunk: (i32, i32),
    /// Horizontal and vertical crown radius.
    crown: (i32, i32),
}

impl Form {
    /// Every `plants.ron` row's rule, in row order; `None` for a row
    /// without `parts` or whose materials the table lacks.
    fn all(tables: &EcoTables) -> Vec<Option<Self>> {
        let m = tables.materials();
        tables
            .plants()
            .iter()
            .map(|p| {
                let parts = p.parts.as_ref()?;
                let FormDef::Tree { trunk, crown } = parts.form;
                Some(Form {
                    root: m.id(&p.material)?,
                    wood: m.id(&parts.wood)?,
                    leaf: m.id(&parts.leaf)?,
                    cost: i32::from(parts.cost),
                    leaf_feed: i32::from(parts.leaf_feed),
                    leaf_light: i32::from(parts.leaf_light),
                    trunk: (i32::from(trunk.0), i32::from(trunk.1)),
                    crown: (i32::from(crown.0), i32::from(crown.1)),
                })
            })
            .collect()
    }

    fn material(&self, kind: PartKind) -> MaterialId {
        match kind {
            PartKind::Leaf => self.leaf,
            _ => self.wood,
        }
    }
}

/// The plant extension sweep (docs/VEGETATION.md V4), built once per
/// season.
#[derive(Clone, Debug)]
pub struct ExtendPass {
    forms: Vec<Option<Form>>,
    air: MaterialId,
    /// Role `water`: a part grows through a puddle.
    water: Option<MaterialId>,
    seed: u64,
}

impl ExtendPass {
    /// The extension of a world of seed `seed`.
    pub fn new(tables: &EcoTables, seed: u64) -> Self {
        Self {
            forms: Form::all(tables),
            air: tables.role_material("air").unwrap_or(MaterialId::AIR),
            water: tables.role_material("water"),
            seed,
        }
    }

    /// Whether any species grows parts.
    pub fn is_inert(&self) -> bool {
        self.forms.iter().all(Option::is_none)
    }

    /// Adds every root of a multi-cell species to the extension active
    /// set, then runs the sweep.
    pub fn run(&mut self, world: &mut World, ledger: &mut SeasonLedger) -> SweepReport {
        let roots: Vec<CellPos> = world
            .iter_chunks_sorted()
            .flat_map(|(pos, chunk)| {
                chunk
                    .plants
                    .keys()
                    .map(move |&i| CellPos::from_chunk_local(pos, LocalPos::from_index(i)))
            })
            .filter(|&cell| self.form_at(world, cell).is_some())
            .collect();
        for cell in roots {
            world.eco_activate(cell, SWEEP_EXTEND);
        }
        run_sweep(world, self, ledger)
    }

    /// The part rule of the root at `cell`: a plant state of a multi-cell
    /// species on its cell of the species' material.
    fn form_at(&self, world: &World, cell: CellPos) -> Option<&Form> {
        let plant = world.plant(cell)?;
        let form = self.forms.get(usize::from(plant.species))?.as_ref()?;
        (world.get_cell(cell) == Some(form.root)).then_some(form)
    }

    /// The tree's seeded trunk height, root included.
    fn trunk_height(&self, form: &Form, root: CellPos) -> i32 {
        let span = form.trunk.1 - form.trunk.0 + 1;
        let roll = i32::from(roll(self.seed, b"trunk", root.0.to_array()));
        form.trunk.0 + roll % span
    }

    /// The next part of the tree at `root` whose cells (root included) are
    /// `members`: its cell, its parent and its kind.
    fn next_part(
        &self,
        world: &World,
        form: &Form,
        root: CellPos,
        members: &BTreeSet<CellPos>,
    ) -> Option<(CellPos, CellPos, PartKind)> {
        // The trunk: trunk parts straight up from the root.
        let mut top = root;
        let mut height = 1;
        loop {
            let up = above(top);
            match Part::from_value(world.layer(up, LayerId::Part)) {
                Some(p) if p.kind == PartKind::Trunk && p.parent(up) == top => {
                    top = up;
                    height += 1;
                }
                _ => break,
            }
        }
        let up = above(top);
        if height < self.trunk_height(form, root) && self.is_open(world, up) {
            return Some((up, top, PartKind::Trunk));
        }

        // The crown: the open cell inside it next to the plant nearest its
        // center.
        let center = up;
        let (r, h) = form.crown;
        let distance = |c: CellPos| {
            let d = c.0 - center.0;
            (d.x * d.x + d.z * d.z) * h * h + d.y * d.y * r * r
        };
        let reach = r * r * h * h;
        let mut best: Option<((i32, u8), CellPos)> = None;
        for &m in members {
            for n in faces(m) {
                if members.contains(&n) || !self.is_open(world, n) {
                    continue;
                }
                let d = distance(n);
                if d > reach {
                    continue;
                }
                let key = (d, roll(self.seed, b"leaf", (n.0 - root.0).to_array()));
                if best.is_none_or(|(k, _)| key < k) {
                    best = Some((key, n));
                }
            }
        }
        let (_, leaf) = best?;
        let parent = faces(leaf).into_iter().find(|n| members.contains(n))?;
        Some((leaf, parent, PartKind::Leaf))
    }

    /// Whether a part may grow into `cell`: air, or water (rain stands
    /// on wood), which the displacement rules push aside.
    fn is_open(&self, world: &World, cell: CellPos) -> bool {
        world.bounds().contains(cell)
            && world
                .get_cell(cell)
                .is_some_and(|m| m == self.air || Some(m) == self.water)
    }
}

impl SweepPass for ExtendPass {
    fn kind(&self) -> usize {
        SWEEP_EXTEND
    }

    fn visit(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos) {
        let Some(form) = self.form_at(ctx.world, cell).cloned() else {
            return;
        };
        let parts = parts_of(ctx.world, cell);
        let lit = parts
            .iter()
            .filter(|&&(c, kind)| {
                kind == PartKind::Leaf && ctx.world.layer(c, LayerId::Light) >= form.leaf_light
            })
            .count();
        let lit = i32::try_from(lit).unwrap_or(i32::MAX);
        let fed = lit.saturating_mul(form.leaf_feed) / FEED_DIV;
        let mut biomass = (ctx.world.layer(cell, LayerId::Biomass) + fed).min(u8::MAX.into());
        if biomass >= form.cost {
            let mut members: BTreeSet<CellPos> = parts.iter().map(|&(c, _)| c).collect();
            members.insert(cell);
            if let Some((at, parent, kind)) = self.next_part(ctx.world, &form, cell, &members) {
                let part = Part::toward(at, parent, kind).expect("a face neighbor");
                ctx.world.eco_set(at, form.material(kind), ctx.ledger);
                ctx.world
                    .eco_put_layer(at, LayerId::Part, i32::from(part.value()));
                biomass -= form.cost;
            }
        }
        ctx.world.eco_put_layer(cell, LayerId::Biomass, biomass);
    }

    fn holds(&self, world: &World, cell: CellPos) -> bool {
        self.form_at(world, cell).is_some()
    }
}

/// The parts of the plant rooted at `root` with their kinds, breadth first
/// from the root, children in [`PART_DIRS`] order.
pub fn parts_of(world: &World, root: CellPos) -> Vec<(CellPos, PartKind)> {
    let mut out = Vec::new();
    let mut queue = VecDeque::from([root]);
    while let Some(c) = queue.pop_front() {
        for n in faces(c) {
            if let Some(p) = Part::from_value(world.layer(n, LayerId::Part))
                && p.parent(n) == c
            {
                out.push((n, p.kind));
                queue.push_back(n);
            }
        }
    }
    out
}

/// The V4 invariant: every part value is well formed, sits on a cell that
/// is not air, and its parent chain reaches a plant root.
pub fn check_parts(world: &World) -> Result<(), String> {
    let mut cells = Vec::new();
    for (pos, chunk) in world.iter_chunks_sorted() {
        if !chunk.layers.contains(LayerId::Part) {
            continue;
        }
        for i in 0..rmc_world::coords::CHUNK_VOLUME {
            let local = LocalPos::from_index(i as u16);
            if chunk.layers.get_u8(LayerId::Part, local) != 0 {
                cells.push(CellPos::from_chunk_local(pos, local));
            }
        }
    }
    for &cell in &cells {
        let mut c = cell;
        for _ in 0..=cells.len() {
            let v = world.layer(c, LayerId::Part);
            let Some(part) = Part::from_value(v) else {
                return Err(format!("{c:?} (on the chain of {cell:?}) holds part {v}"));
            };
            if world.get_cell(c).is_none_or(|m| m == MaterialId::AIR) {
                return Err(format!("{c:?} is a part of air"));
            }
            c = part.parent(c);
            if world.plant(c).is_some() {
                break;
            }
        }
        if world.plant(c).is_none() {
            return Err(format!("the parent chain of {cell:?} loops"));
        }
    }
    Ok(())
}

/// The six face neighbors in [`PART_DIRS`] order.
fn faces(c: CellPos) -> [CellPos; 6] {
    PART_DIRS.map(|[x, y, z]| CellPos::new(c.0.x + x, c.0.y + y, c.0.z + z))
}

fn above(c: CellPos) -> CellPos {
    CellPos::new(c.0.x, c.0.y + 1, c.0.z)
}

/// A seeded byte of `(tag, v)`.
fn roll(seed: u64, tag: &[u8], v: [i32; 3]) -> u8 {
    let mut h = Xxh3::with_seed(seed);
    h.update(tag);
    for x in v {
        h.update(&x.to_le_bytes());
    }
    h.digest().to_le_bytes()[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;
    use std::sync::Arc;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, ChunkPos};
    use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId};
    use rmc_world::layers::PlantState;
    use rmc_world::shape::Shape;

    use crate::activity::Activity;
    use crate::invariants::InvariantChecker;
    use crate::plants::GrowthPass;
    use crate::runner::{Exec, activate_holders, run_jacobi};
    use crate::tables::EcoSources;
    use crate::tick::season_tick_observed;
    use crate::water::WaterPass;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    const FED: [(&str, i32); 3] = [("n", 50), ("p", 50), ("k", 50)];
    const SEED: u64 = 7;

    fn tables() -> EcoTables {
        EcoTables::load(ASSETS).expect("Phase 2 tables load")
    }

    /// The shipped tables with the oak's trunk height fixed at `h`.
    fn tables_with_trunk(h: u8) -> EcoTables {
        let shipped = tables();
        let mut src = EcoSources::read(ASSETS).unwrap();
        let from = "trunk: (4, 6)";
        assert!(src.plants.contains(from));
        src.plants = src.plants.replace(from, &format!("trunk: ({h}, {h})"));
        EcoTables::from_sources(Arc::clone(shipped.materials()), &src).expect("edited oak loads")
    }

    fn id(t: &EcoTables, key: &str) -> MaterialId {
        t.materials().id(key).unwrap()
    }

    fn species(t: &EcoTables, key: &str) -> u8 {
        t.plants().iter().position(|p| p.key == key).unwrap() as u8
    }

    /// Four chunks: two side by side along x (the border runs between
    /// x = 31 and x = 32), loam (which drains the rain) up to y = 30 and
    /// air from y = 31, and two of air above them.
    fn world(t: &EcoTables) -> World {
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 63, 31));
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        let mut soil = Chunk::uniform(id(t, "loam"));
        for z in 0..32 {
            for x in 0..32 {
                soil.storage
                    .set(CellPos::new(x, 31, z).split().1, MaterialId::AIR);
            }
        }
        for cx in 0..2 {
            w.insert_chunk(ChunkPos::new(cx, 0, 0), soil.clone())
                .unwrap();
            w.insert_chunk(ChunkPos::new(cx, 1, 0), Chunk::uniform(MaterialId::AIR))
                .unwrap();
        }
        w
    }

    /// An oak root at `cell` (y 31) on fed, moist loam, 20.0 °C.
    fn oak(w: &mut World, t: &EcoTables, cell: CellPos, growth: i32) {
        let mut scratch = SeasonLedger::default();
        w.eco_set(cell, id(t, "oak_wood"), &mut scratch);
        w.eco_put_plant(
            cell,
            Some(PlantState {
                species: species(t, "oak"),
                ..PlantState::default()
            }),
        );
        w.eco_put_layer(cell, LayerId::Growth, growth);
        w.eco_put_layer(cell, LayerId::Temperature, 200);
        let anchor = CellPos::new(cell.0.x, cell.0.y - 1, cell.0.z);
        w.eco_put_layer(anchor, LayerId::Moisture, 100);
        for (x, n) in FED {
            let x = t.substance_index(x).unwrap();
            w.eco_put_layer(anchor, LayerId::Substance(x), n);
        }
    }

    /// One extension sweep, checking the conservation invariants and the
    /// parts invariant.
    fn extend(w: &mut World, t: &EcoTables) -> SeasonLedger {
        let checker = InvariantChecker::new(w);
        let mut ledger = SeasonLedger::default();
        ExtendPass::new(t, SEED).run(w, &mut ledger);
        checker.check(w, &ledger).unwrap_or_else(|v| panic!("{v}"));
        check_parts(w).unwrap();
        ledger
    }

    /// Extension sweeps with the root's biomass topped up each time, until
    /// the tree stops growing; returns how many parts each sweep added.
    fn grow_out(w: &mut World, t: &EcoTables, root: CellPos) -> Vec<usize> {
        let mut added = Vec::new();
        loop {
            let before = parts_of(w, root).len();
            w.eco_put_layer(root, LayerId::Biomass, 255);
            extend(w, t);
            let n = parts_of(w, root).len() - before;
            if n == 0 {
                return added;
            }
            added.push(n);
            assert!(added.len() < 200, "the tree stops growing");
        }
    }

    fn form(t: &EcoTables) -> (u8, u8, u8, u8) {
        let oak = &t.plants()[usize::from(species(t, "oak"))];
        let FormDef::Tree { trunk, crown } = oak.parts.as_ref().unwrap().form;
        (trunk.0, trunk.1, crown.0, crown.1)
    }

    /// The tree at `root` by offset from the root.
    fn shape(w: &World, root: CellPos) -> BTreeMap<[i32; 3], PartKind> {
        parts_of(w, root)
            .into_iter()
            .map(|(c, k)| ((c.0 - root.0).to_array(), k))
            .collect()
    }

    /// A grown tree's trunk height, root included.
    fn trunk_height(shape: &BTreeMap<[i32; 3], PartKind>) -> i32 {
        1 + shape.values().filter(|&&k| k == PartKind::Trunk).count() as i32
    }

    #[test]
    fn a_tree_grows_its_trunk_then_fills_its_crown() {
        let t = tables();
        let (lo, hi, r, h) = form(&t);
        let (r, h) = (i32::from(r), i32::from(h));
        let mut w = world(&t);
        let root = CellPos::new(10, 31, 10);
        oak(&mut w, &t, root, 255);

        // One part per sweep, until the crown is full.
        let added = grow_out(&mut w, &t, root);
        assert!(added.iter().all(|&n| n == 1), "{added:?}");
        let tree = shape(&w, root);
        let height = trunk_height(&tree);
        assert!(
            (i32::from(lo)..=i32::from(hi)).contains(&height),
            "{height}"
        );
        // The trunk is a straight column, built first.
        for y in 1..height {
            assert_eq!(tree.get(&[0, y, 0]), Some(&PartKind::Trunk), "trunk {y}");
        }
        // Every cell of the crown is a leaf, and every leaf in the crown.
        let top = height - 1;
        let in_crown = |[x, y, z]: [i32; 3]| {
            let dy = y - top - 1;
            (x * x + z * z) * h * h + dy * dy * r * r <= r * r * h * h
        };
        let leaves: Vec<[i32; 3]> = tree
            .iter()
            .filter(|&(_, &k)| k == PartKind::Leaf)
            .map(|(&o, _)| o)
            .collect();
        assert!(leaves.iter().all(|&o| in_crown(o)));
        let mut crown = 0;
        for y in top - h..=top + 1 + h {
            for z in -r..=r {
                for x in -r..=r {
                    if in_crown([x, y, z]) && !(x == 0 && z == 0 && y <= top) {
                        crown += 1;
                    }
                }
            }
        }
        assert_eq!(leaves.len(), crown);
        // The cells hold the species' materials.
        for (&[x, y, z], &k) in &tree {
            let c = CellPos::new(root.0.x + x, root.0.y + y, root.0.z + z);
            let want = if k == PartKind::Leaf {
                "oak_leaves"
            } else {
                "oak_wood"
            };
            assert_eq!(w.get_cell(c), Some(id(&t, want)));
        }
        // A full tree spends nothing.
        w.eco_put_layer(root, LayerId::Biomass, 200);
        extend(&mut w, &t);
        assert_eq!(w.layer(root, LayerId::Biomass), 200);
    }

    #[test]
    fn a_part_costs_biomass_and_lit_leaves_make_it() {
        let t = tables();
        let oak_row = &t.plants()[usize::from(species(&t, "oak"))];
        let parts = oak_row.parts.clone().unwrap();
        let (cost, feed) = (i32::from(parts.cost), i32::from(parts.leaf_feed));
        let mut w = world(&t);
        let root = CellPos::new(10, 31, 10);
        oak(&mut w, &t, root, 255);

        // Short of the cost: nothing grows, nothing is spent.
        w.eco_put_layer(root, LayerId::Biomass, cost - 1);
        extend(&mut w, &t);
        assert!(parts_of(&w, root).is_empty());
        assert_eq!(w.layer(root, LayerId::Biomass), cost - 1);
        // At the cost: one part, paid for.
        w.eco_put_layer(root, LayerId::Biomass, cost + 3);
        extend(&mut w, &t);
        assert_eq!(parts_of(&w, root).len(), 1);
        assert_eq!(w.layer(root, LayerId::Biomass), 3);

        // A full tree: its lit leaves feed it, its shaded ones do not.
        grow_out(&mut w, &t, root);
        let leaves: Vec<CellPos> = parts_of(&w, root)
            .into_iter()
            .filter(|&(_, k)| k == PartKind::Leaf)
            .map(|(c, _)| c)
            .collect();
        let lit = 5;
        for (i, &c) in leaves.iter().enumerate() {
            let light = if i < lit {
                i32::from(parts.leaf_light)
            } else {
                i32::from(parts.leaf_light) - 1
            };
            w.eco_put_layer(c, LayerId::Light, light);
        }
        w.eco_put_layer(root, LayerId::Biomass, 0);
        extend(&mut w, &t);
        assert_eq!(
            w.layer(root, LayerId::Biomass),
            lit as i32 * feed / FEED_DIV
        );
    }

    #[test]
    fn growth_past_maturity_becomes_biomass() {
        let t = tables();
        let gps = i32::from(t.plants()[usize::from(species(&t, "oak"))].growth_per_substep);
        let mut w = world(&t);
        let root = CellPos::new(10, 31, 10);
        oak(&mut w, &t, root, 250);
        // A one-cell plant at 250 gains no biomass.
        let moss = CellPos::new(20, 31, 20);
        let mut scratch = SeasonLedger::default();
        w.eco_set(moss, id(&t, "moss"), &mut scratch);
        w.eco_put_plant(
            moss,
            Some(PlantState {
                species: species(&t, "moss"),
                ..PlantState::default()
            }),
        );
        w.eco_put_layer(moss, LayerId::Growth, 250);
        w.eco_put_layer(moss, LayerId::Light, 255);
        w.eco_put_layer(moss, LayerId::Temperature, 200);
        w.eco_put_layer(CellPos::new(20, 30, 20), LayerId::Moisture, 100);

        let mut ledger = SeasonLedger::default();
        run_jacobi(&mut w, &GrowthPass::new(&t), Exec::Serial, &mut ledger);
        assert_eq!(w.plant(root).map(|p| p.stress), Some(0), "needs met");
        assert_eq!(w.layer(root, LayerId::Growth), 255);
        assert_eq!(w.layer(root, LayerId::Biomass), gps - 5);
        assert_eq!(w.layer(moss, LayerId::Growth), 255);
        assert_eq!(w.layer(moss, LayerId::Biomass), 0);
        run_jacobi(&mut w, &GrowthPass::new(&t), Exec::Serial, &mut ledger);
        assert_eq!(w.layer(root, LayerId::Biomass), 2 * gps - 5);
    }

    #[test]
    fn cutting_a_part_cuts_off_exactly_what_hangs_from_it() {
        let t = tables();
        let mut w = world(&t);
        let root = CellPos::new(10, 31, 10);
        oak(&mut w, &t, root, 255);
        grow_out(&mut w, &t, root);
        let tree = shape(&w, root);
        assert!(tree.len() > 10);

        // The pickaxe on the second trunk cell: the first stays, the rest
        // of the tree is air and no longer a part.
        let cut = CellPos::new(10, 33, 10);
        let issuer = PlayerId(1);
        let r = w.apply(&EditCommand {
            id: CommandId { issuer, seq: 1 },
            issuer,
            op: EditOp::Damage(u16::MAX),
            shape: Shape::Cell(cut),
        });
        assert_eq!(r.status, EditStatus::Applied);
        check_parts(&w).unwrap();
        assert_eq!(shape(&w, root).keys().collect::<Vec<_>>(), [&[0, 1, 0]]);
        for &[x, y, z] in tree.keys() {
            let c = CellPos::new(root.0.x + x, root.0.y + y, root.0.z + z);
            if y >= 2 {
                assert_eq!(w.get_cell(c), Some(MaterialId::AIR), "{c:?}");
                assert_eq!(w.layer(c, LayerId::Part), 0, "{c:?}");
            }
        }
        // It grows back from what is left.
        grow_out(&mut w, &t, root);
        assert_eq!(shape(&w, root), tree, "the same tree");

        // The root's death takes the tree.
        let mut ledger = SeasonLedger::default();
        w.eco_set(root, MaterialId::AIR, &mut ledger);
        check_parts(&w).unwrap();
        assert!(w.plant(root).is_none());
        for &[x, y, z] in tree.keys() {
            let c = CellPos::new(root.0.x + x, root.0.y + y, root.0.z + z);
            assert_eq!(w.get_cell(c), Some(MaterialId::AIR), "{c:?}");
        }
    }

    #[test]
    fn a_cut_off_part_leaves_litter_and_the_ledger_balances() {
        let t = tables();
        let litter = i32::from(t.params().part_litter.unwrap());
        assert!(litter > 0);
        let organic = LayerId::Substance(t.role_substance("organic").unwrap());
        let mut w = world(&t);
        let root = CellPos::new(10, 31, 10);
        oak(&mut w, &t, root, 255);
        // A low branch beside the root, over the soil, and a leaf on it.
        let mut scratch = SeasonLedger::default();
        let branch = CellPos::new(11, 31, 10);
        let leaf = CellPos::new(11, 32, 10);
        for (c, parent, kind, m) in [
            (branch, root, PartKind::Branch, "oak_wood"),
            (leaf, branch, PartKind::Leaf, "oak_leaves"),
        ] {
            w.eco_set(c, id(&t, m), &mut scratch);
            let part = Part::toward(c, parent, kind).unwrap();
            w.eco_put_layer(c, LayerId::Part, i32::from(part.value()));
        }
        check_parts(&w).unwrap();
        let soil = CellPos::new(11, 30, 10);
        let had = w.layer(soil, organic);

        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        w.eco_set(root, MaterialId::AIR, &mut ledger);
        checker.check(&w, &ledger).unwrap_or_else(|v| panic!("{v}"));
        check_parts(&w).unwrap();
        assert_eq!(w.get_cell(branch), Some(MaterialId::AIR));
        assert_eq!(w.get_cell(leaf), Some(MaterialId::AIR));
        // The branch sat on soil: its litter is there. The leaf sat on the
        // branch, which was gone by then: no anchor, no litter.
        assert_eq!(w.layer(soil, organic), had + litter);
    }

    #[test]
    fn a_tree_across_a_chunk_border_grows_like_one_inside_a_chunk() {
        // Rule 20, for a sweep: the tree at x 31 has its crown in both
        // chunks; the one at x 10 in one. The trunk height is fixed, so the
        // trees differ only in where they stand.
        let t = tables_with_trunk(5);
        let (inside, across) = (CellPos::new(10, 31, 10), CellPos::new(31, 31, 20));
        let grown = || {
            let mut w = world(&t);
            oak(&mut w, &t, inside, 255);
            oak(&mut w, &t, across, 255);
            for _ in 0..80 {
                for root in [inside, across] {
                    w.eco_put_layer(root, LayerId::Biomass, 255);
                }
                extend(&mut w, &t);
            }
            w
        };
        let w = grown();
        let a = shape(&w, across);
        assert!(
            a.keys().any(|o| across.0.x + o[0] >= 32),
            "crosses the border"
        );
        assert_eq!(a, shape(&w, inside));
        assert_eq!(trunk_height(&a), 5);
        // Determinism: the same seed grows the same world.
        assert_eq!(grown().hash(), w.hash());
    }

    #[test]
    fn a_tree_grows_through_seasons_and_its_parts_reach_its_root() {
        let t = tables();
        let mut w = world(&t);
        let root = CellPos::new(10, 31, 10);
        oak(&mut w, &t, root, 200);
        activate_holders(&mut w, &WaterPass::new(&t));
        let mut activity = Activity::new(t.params().quiet_skip);
        for season in 0..2 {
            let checker = InvariantChecker::new(&w);
            season_tick_observed(
                &mut w,
                &t,
                season,
                SEED,
                &mut activity,
                Exec::Serial,
                &mut |i, pass, w, ledger| {
                    let at = format!("season {season}, sub-step {i}, after {pass:?}");
                    if let Err(v) = checker.check(w, ledger) {
                        panic!("{at}: {v}");
                    }
                    if let Err(e) = check_parts(w) {
                        panic!("{at}: {e}");
                    }
                },
            );
        }
        assert!(w.plant(root).is_some(), "the oak lives");
        let tree = shape(&w, root);
        assert!(tree.len() >= 4, "it grew: {tree:?}");
        assert_eq!(tree.get(&[0, 1, 0]), Some(&PartKind::Trunk));
    }
}
