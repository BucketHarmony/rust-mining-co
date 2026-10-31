//! Multi-cell plants (docs/VEGETATION.md V4, V5, V6): trees and vines grow
//! parts.
//!
//! A multi-cell plant is a `plants.ron` row with `parts`. Its root is the
//! plant cell (with a [`PlantState`] and the plant rules of
//! [`crate::plants`]): first the row's material, the seed, which grows as a
//! seedling, then the row's `wood` once it ripens. Every other cell of it
//! is a part: a cell of the row's `wood`, `leaf`, flower or seed (fruit)
//! material whose [`LayerId::Part`] value names its kind and the face
//! neighbor it hangs from. Parent chains end at the root, so the plant is
//! the tree of cells reachable from it.
//!
//! **Biomass.** The growth pass puts a root's growth past 255 into its
//! [`LayerId::Biomass`]; each extension step adds `lit · leaf_feed / 256`,
//! where `lit` counts the leaves and vine cells whose light is at least
//! `leaf_light`.
//!
//! **Extension (sweep, after spread)**, per root in the global cell order,
//! on the sub-steps whose calendar hour `part_hours` divides (every
//! sub-step of a `seasons.ron` season):
//! 1. In the season its fruit falls, each fruit drops to the first cell
//!    below it that is neither open nor a part nor a plant (so through the
//!    crown and the trunk to the soil). When the seeded roll of
//!    `(seed, season, fruit cell)` is below `take`, the open cell above
//!    that landing is air, and the species may start there
//!    ([`PlantStarts::passes`]), a seed plant starts there (ledger
//!    `plants_born`); otherwise the fruit rots on the landing as
//!    `eco.part_litter` organic when the landing is an anchor material
//!    (ledger `litter_in`). The fruit cell becomes air.
//! 2. In its `shed` season the plant is bare: every leaf, flower and fruit
//!    left falls the same way as litter, children before parents, and
//!    nothing grows that season. Out of the flower's seasons, its flowers
//!    wilt the same way.
//! 3. Otherwise the lit leaves feed it, and, when the biomass is at least
//!    `cost`, it pays `cost` for one step: a seed root ripens into the
//!    wood (`World::eco_set`; the §7.0 rules keep the plant), else the
//!    next part of the form grows, else, in a flower season and below the
//!    flower's `count`, a flower opens on an open cell beside a lit leaf or
//!    vine cell (the least seeded roll), else, in the fruit's `grow` season
//!    and below its `count`, a fruit grows on an open cell beside a leaf (the
//!    least seeded roll of its offset from the root). A new part is placed
//!    at once (`World::eco_set`, then its part value), so roots later in
//!    the order see it. A part grows into an open cell: air, or water
//!    (rain stands on wood), which the §7.0 rules push aside.
//!
//! **Tree form.** The trunk grows straight up from the root while its
//! height (root included) is below the tree's seeded height in `trunk`
//! and the cell above is open. Then the crown fills, an ellipsoid of
//! horizontal radius `crown.0` and vertical radius `crown.1` centered on
//! the cell above the trunk's top: the next part is the open cell in the
//! crown, face-adjacent to the plant, nearest the center (ties broken by a
//! seeded hash of its offset from the root, so the shape does not depend
//! on where the tree stands). With `branches`, the center and the cells on
//! the crown's horizontal axes through it, short of the radius, are
//! branches hanging from their first wood neighbor in [`PART_DIRS`] order
//! (a cell with no wood neighbor yet is a leaf); every other part is a
//! leaf hanging from its first neighbor in that order that belongs to the
//! plant.
//!
//! **Vine form.** Below `reach` vine cells, the next part is an open cell
//! beside the root or a vine cell that touches a material with one of
//! `climb_tags` outside the plant along a face or an edge (so a vine
//! crests a wall); up before sideways before down, ties by a seeded roll
//! of its offset from the root. With `hang`, a cell
//! straight below a supported or hanging vine cell needs no support, so a
//! vine hangs over a ledge as a curtain. A vine without support stops.
//!
//! **Graph form** (docs/TREES.md): the tree grows no part cells. Its lit
//! leafy metamers feed it like leaves, and step 3 grows its metamer graph
//! by [`crate::trees::grow`] with every whole `cost` of the biomass,
//! paying `cost` per metamer added. A graph whose root is no longer a
//! graph tree's is removed before the sweep.
//!
//! A graph tree's root that still carries part cells (a save from before
//! the row grew graphs, docs/TREES.md T7) drops them first, children
//! before parents, each rotting as `eco.part_litter` on its landing like a
//! bare season's leaves; the graph then grows from the root.
//!
//! Its flowers and fruit (T6) are Flower and Fruit buds on its tips
//! ([`crate::trees::bear`]). Steps 1 and 2 drop them as they drop part
//! cells, from the cell their metamer ends in: fruit falls (and may take
//! root) in its `fall` season, flowers wilt out of their seasons, and a
//! bare season drops both as litter; the bud turns Active again. In step
//! 3 a tree of at least `mature` metamers first opens flowers (in a
//! flower season, on tips with `leaf_light`), then sets fruit (in the
//! fruit's `grow` season, on any leafy tip), each up to its `count` and
//! paying `cost`, the tips by the least seeded roll; what is left grows.
//!
//! **Severing** is a displacement rule (`rmc_world::displace`): a cell
//! whose material changes stops being a part, and the parts hanging from it
//! become air in turn, each leaving `eco.part_litter` in the anchor below
//! it. Felling the trunk takes the crown; a dead root takes the tree.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use rmc_world::coords::{CellPos, LocalPos};
use rmc_world::layers::{LayerId, PART_DIRS, Part, PartKind, PlantState, SWEEP_EXTEND};
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::{MaterialId, MaterialTable};
use rmc_world::tree::BudState;
use rmc_world::world::World;
use xxhash_rust::xxh3::Xxh3;

use crate::plants::PlantStarts;
use crate::runner::{SweepCtx, SweepPass, SweepReport, run_sweep};
use crate::tables::{EcoTables, FormDef};
use crate::trees::{self, Growth, Site};

/// The divisor of `leaf_feed` (256ths of biomass per lit leaf).
const FEED_DIV: i32 = 256;

/// One multi-cell species' part rule, resolved to ids and widened.
#[derive(Clone, Debug)]
struct Form {
    /// The row's material: the seed a root starts as, and the fruit.
    seed: MaterialId,
    wood: MaterialId,
    leaf: MaterialId,
    cost: i32,
    leaf_feed: i32,
    leaf_light: i32,
    shape: Shape,
    /// The season row in which the tree is bare.
    shed: Option<usize>,
    fruit: Option<Fruit>,
    flower: Option<Flower>,
}

/// What a multi-cell plant grows toward.
#[derive(Clone, Debug)]
enum Shape {
    Tree {
        /// Inclusive trunk height range, root included.
        trunk: (i32, i32),
        /// Horizontal and vertical crown radius.
        crown: (i32, i32),
        branches: bool,
    },
    Vine {
        /// Per material id: whether a vine climbs its faces.
        climb: Vec<bool>,
        reach: usize,
        hang: bool,
    },
    /// A tree as geometry (docs/TREES.md): no part cells, a metamer graph.
    Graph(Growth),
}

/// A plant's flower rule, with season rows.
#[derive(Clone, Debug)]
struct Flower {
    material: MaterialId,
    rows: Vec<usize>,
    count: usize,
}

/// A tree's fruit rule, with season rows.
#[derive(Clone, Copy, Debug)]
struct Fruit {
    grow: usize,
    fall: usize,
    count: usize,
    take: u8,
}

impl Form {
    /// Every `plants.ron` row's rule, in row order; `None` for a row
    /// without `parts` or whose materials the table lacks.
    fn all(tables: &EcoTables) -> Vec<Option<Self>> {
        let m = tables.materials();
        let row = |key: &str| tables.seasons().seasons.iter().position(|s| s.key == key);
        tables
            .plants()
            .iter()
            .map(|p| {
                let parts = p.parts.as_ref()?;
                let shape = match &parts.form {
                    FormDef::Tree { trunk, crown } => Shape::Tree {
                        trunk: (i32::from(trunk.0), i32::from(trunk.1)),
                        crown: (i32::from(crown.0), i32::from(crown.1)),
                        branches: parts.branches,
                    },
                    FormDef::Vine {
                        climb_tags,
                        reach,
                        hang,
                    } => Shape::Vine {
                        climb: m
                            .iter()
                            .map(|d| climb_tags.iter().any(|t| d.has_tag(t)))
                            .collect(),
                        reach: usize::from(*reach),
                        hang: *hang,
                    },
                    FormDef::Graph { .. } => Shape::Graph(Growth::from_form(&parts.form)?),
                };
                let flower = match &parts.flower {
                    Some(f) => Some(Flower {
                        material: m.id(&f.material)?,
                        rows: f.seasons.iter().map(|k| row(k)).collect::<Option<_>>()?,
                        count: usize::from(f.count),
                    }),
                    None => None,
                };
                let fruit = match &parts.fruit {
                    Some(f) => Some(Fruit {
                        grow: row(&f.grow)?,
                        fall: row(&f.fall)?,
                        count: usize::from(f.count),
                        take: f.take,
                    }),
                    None => None,
                };
                Some(Form {
                    seed: m.id(&p.material)?,
                    wood: m.id(&parts.wood)?,
                    leaf: m.id(&parts.leaf)?,
                    cost: i32::from(parts.cost),
                    leaf_feed: i32::from(parts.leaf_feed),
                    leaf_light: i32::from(parts.leaf_light),
                    shape,
                    shed: parts.shed.as_deref().and_then(row),
                    fruit,
                    flower,
                })
            })
            .collect()
    }

    fn material(&self, kind: PartKind) -> MaterialId {
        match kind {
            PartKind::Leaf | PartKind::Vine => self.leaf,
            PartKind::Fruit => self.seed,
            PartKind::Flower => self.flower.as_ref().map_or(self.leaf, |f| f.material),
            PartKind::Trunk | PartKind::Branch => self.wood,
        }
    }

    /// Whether a part of `kind` makes biomass when lit: leaves and vine
    /// cells.
    fn feeds(kind: PartKind) -> bool {
        matches!(kind, PartKind::Leaf | PartKind::Vine)
    }

    /// Whether the root cell's material is this species' body.
    fn is_root(&self, m: Option<MaterialId>) -> bool {
        m == Some(self.seed) || m == Some(self.wood)
    }
}

/// The plant extension sweep (docs/VEGETATION.md V4, V5), built once per
/// season.
#[derive(Clone, Debug)]
pub struct ExtendPass {
    forms: Vec<Option<Form>>,
    air: MaterialId,
    /// Role `water`: a part grows through a puddle.
    water: Option<MaterialId>,
    /// Role `organic`: what fallen leaves and rotten fruit leave.
    organic: Option<u8>,
    part_litter: i32,
    /// `eco.metamer_litter`: what a fallen leaf or a shed metamer leaves.
    metamer_litter: i32,
    materials: Arc<MaterialTable>,
    starts: PlantStarts,
    season: u32,
    /// The season's `seasons.ron` row.
    row: usize,
    seed: u64,
}

impl ExtendPass {
    /// The extension of a world of seed `seed` in season 0.
    pub fn new(tables: &EcoTables, seed: u64) -> Self {
        Self::for_season(tables, 0, seed)
    }

    /// The extension of a world of seed `seed` in season `season`.
    pub fn for_season(tables: &EcoTables, season: u32, seed: u64) -> Self {
        let rows = tables.seasons().seasons.len().max(1);
        Self {
            forms: Form::all(tables),
            air: tables.role_material("air").unwrap_or(MaterialId::AIR),
            water: tables.role_material("water"),
            organic: tables.role_substance("organic"),
            part_litter: i32::from(tables.params().part_litter.unwrap_or(0)),
            metamer_litter: i32::from(tables.params().metamer_litter.unwrap_or(0)),
            materials: Arc::clone(tables.materials()),
            starts: PlantStarts::new(tables),
            season,
            row: usize::try_from(season).unwrap_or(usize::MAX) % rows,
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
        // A graph whose root is no longer a graph tree's goes with it.
        let orphans: Vec<CellPos> = world
            .iter_chunks_sorted()
            .flat_map(|(pos, chunk)| {
                chunk
                    .trees
                    .keys()
                    .map(move |&i| CellPos::from_chunk_local(pos, LocalPos::from_index(i)))
            })
            .filter(|&cell| {
                !self
                    .form_at(world, cell)
                    .is_some_and(|(_, f)| matches!(f.shape, Shape::Graph(_)))
            })
            .collect();
        for cell in orphans {
            world.eco_put_tree(cell, None);
        }
        run_sweep(world, self, ledger)
    }

    /// What the tree rooted at `root` reads of `world`: the light layer,
    /// and where an internode may end (an open cell, a part or a plant).
    fn site<'w>(
        &'w self,
        world: &'w World,
        root: CellPos,
    ) -> Site<impl Fn(CellPos) -> i32 + 'w, impl Fn(CellPos) -> bool + 'w> {
        Site {
            root,
            light: move |c| world.layer(c, LayerId::Light),
            passable: move |c| {
                self.is_open(world, c)
                    || (world.bounds().contains(c)
                        && (world.layer(c, LayerId::Part) != 0 || world.plant(c).is_some()))
            },
        }
    }

    /// The species and part rule of the root at `cell`: a plant state of a
    /// multi-cell species on its seed or its wood.
    fn form_at(&self, world: &World, cell: CellPos) -> Option<(u8, &Form)> {
        let plant = world.plant(cell)?;
        let form = self.forms.get(usize::from(plant.species))?.as_ref()?;
        form.is_root(world.get_cell(cell))
            .then_some((plant.species, form))
    }

    /// The tree's seeded trunk height, root included.
    fn trunk_height(&self, trunk: (i32, i32), root: CellPos) -> i32 {
        let span = trunk.1 - trunk.0 + 1;
        let roll = i32::from(roll(self.seed, b"trunk", root.0.to_array()));
        trunk.0 + roll % span
    }

    /// The next part of the plant at `root` whose cells are `members` (the
    /// root's kind `None`): its cell, its parent and its kind.
    fn next_part(
        &self,
        world: &World,
        form: &Form,
        root: CellPos,
        members: &BTreeMap<CellPos, Option<PartKind>>,
    ) -> Option<(CellPos, CellPos, PartKind)> {
        match &form.shape {
            &Shape::Tree {
                trunk,
                crown,
                branches,
            } => self.next_tree_part(world, (trunk, crown, branches), root, members),
            Shape::Vine { climb, reach, hang } => {
                self.next_vine(world, climb, *reach, *hang, root, members)
            }
            Shape::Graph(_) => None,
        }
    }

    /// [`ExtendPass::next_part`] for a tree of `(trunk, crown, branches)`.
    fn next_tree_part(
        &self,
        world: &World,
        (trunk, crown, branches): ((i32, i32), (i32, i32), bool),
        root: CellPos,
        members: &BTreeMap<CellPos, Option<PartKind>>,
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
        if height < self.trunk_height(trunk, root) && self.is_open(world, up) {
            return Some((up, top, PartKind::Trunk));
        }

        // The crown: the open cell inside it next to the plant nearest its
        // center.
        let center = up;
        let (r, h) = crown;
        let distance = |c: CellPos| {
            let d = c.0 - center.0;
            (d.x * d.x + d.z * d.z) * h * h + d.y * d.y * r * r
        };
        let reach = r * r * h * h;
        let mut best: Option<((i32, u8), CellPos)> = None;
        for &m in members.keys() {
            for n in faces(m) {
                if members.contains_key(&n) || !self.is_open(world, n) {
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
        let (_, at) = best?;
        let d = at.0 - center.0;
        let on_axis = d.y == 0 && (d.x == 0 || d.z == 0) && d.x.abs().max(d.z.abs()) < r;
        let wood = |k: &Option<PartKind>| {
            k.is_none_or(|k| matches!(k, PartKind::Trunk | PartKind::Branch))
        };
        if branches
            && on_axis
            && let Some(parent) = faces(at)
                .into_iter()
                .find(|n| members.get(n).is_some_and(wood))
        {
            return Some((at, parent, PartKind::Branch));
        }
        let parent = faces(at).into_iter().find(|n| members.contains_key(n))?;
        Some((at, parent, PartKind::Leaf))
    }

    /// The next vine cell of the plant at `root` whose cells are
    /// `members`: below `reach` vine cells, the open cell beside the root
    /// or a vine cell that touches a `climb` material outside the plant
    /// along a face or an edge (so a vine crests a wall), growing up before
    /// sideways before down (ties by a seeded
    /// roll of its offset from the root). With `hang`, a cell straight
    /// below a supported or hanging vine cell needs no support.
    fn next_vine(
        &self,
        world: &World,
        climb: &[bool],
        reach: usize,
        hang: bool,
        root: CellPos,
        members: &BTreeMap<CellPos, Option<PartKind>>,
    ) -> Option<(CellPos, CellPos, PartKind)> {
        let vines = members
            .values()
            .filter(|k| **k == Some(PartKind::Vine))
            .count();
        if vines >= reach {
            return None;
        }
        let supported = |c: CellPos| {
            touching(c).any(|n| {
                !members.contains_key(&n)
                    && world
                        .get_cell(n)
                        .is_some_and(|m| climb.get(usize::from(m.0)).copied().unwrap_or(false))
            })
        };
        let hanging = |c: CellPos| {
            Part::from_value(world.layer(c, LayerId::Part))
                .is_some_and(|p| p.kind == PartKind::Vine && p.parent(c) == above(c))
        };
        let mut best: Option<((u8, u8, CellPos), CellPos)> = None;
        let stems = members
            .iter()
            .filter(|(_, k)| k.is_none_or(|k| k == PartKind::Vine));
        for (&m, _) in stems {
            for n in faces(m) {
                if members.contains_key(&n) || !self.is_open(world, n) {
                    continue;
                }
                let rank: u8 = match n.0.y - m.0.y {
                    1 => 0,
                    0 => 1,
                    _ => 2,
                };
                let hangs = hang && rank == 2 && (supported(m) || hanging(m));
                if !hangs && !supported(n) {
                    continue;
                }
                let key = (rank, roll(self.seed, b"vine", (n.0 - root.0).to_array()), n);
                if best.is_none_or(|(k, _)| key < k) {
                    best = Some((key, m));
                }
            }
        }
        best.map(|((_, _, at), parent)| (at, parent, PartKind::Vine))
    }

    /// Where a fruit or flower (`what`) grows on the plant at `root`: the
    /// open cell beside one of its leaves or vine cells for which `pick`
    /// holds, with the least seeded roll of `what` and its offset from the
    /// root, and that leaf.
    fn next_beside(
        &self,
        world: &World,
        root: CellPos,
        members: &BTreeMap<CellPos, Option<PartKind>>,
        what: &[u8],
        pick: impl Fn(CellPos) -> bool,
    ) -> Option<(CellPos, CellPos)> {
        let mut best: Option<((u8, CellPos), CellPos)> = None;
        let leaves = members
            .iter()
            .filter(|(c, k)| k.is_some_and(Form::feeds) && pick(**c));
        for (&leaf, _) in leaves {
            for n in faces(leaf) {
                if members.contains_key(&n) || !self.is_open(world, n) {
                    continue;
                }
                let key = (roll(self.seed, what, (n.0 - root.0).to_array()), n);
                if best.is_none_or(|(k, _)| key < k) {
                    best = Some((key, leaf));
                }
            }
        }
        best.map(|((_, at), leaf)| (at, leaf))
    }

    /// Whether a part may grow into `cell`: air, or water (rain stands
    /// on wood), which the displacement rules push aside.
    fn is_open(&self, world: &World, cell: CellPos) -> bool {
        world.bounds().contains(cell)
            && world
                .get_cell(cell)
                .is_some_and(|m| m == self.air || Some(m) == self.water)
    }

    /// The first cell below `from` that is neither open nor a part nor a
    /// plant: where something falling from `from` lands.
    fn landing(&self, world: &World, from: CellPos) -> Option<CellPos> {
        let floor = world.bounds().min.0.y;
        (floor..from.0.y)
            .rev()
            .map(|y| CellPos::new(from.0.x, y, from.0.z))
            .find(|&c| {
                !self.is_open(world, c)
                    && world.layer(c, LayerId::Part) == 0
                    && world.plant(c).is_none()
            })
    }

    /// Lays `eco.part_litter` organic on `landing` when it is an anchor
    /// material, up to its room (ledger `litter_in`).
    fn rot(&self, ctx: &mut SweepCtx<'_>, landing: Option<CellPos>) {
        self.rot_by(ctx, landing, self.part_litter);
    }

    /// Lays `amount` organic on `landing` when it is an anchor material, up
    /// to its room (ledger `litter_in`).
    fn rot_by(&self, ctx: &mut SweepCtx<'_>, landing: Option<CellPos>, amount: i32) {
        let (Some(c), Some(landing)) = (self.organic, landing) else {
            return;
        };
        let anchor = ctx
            .world
            .get_cell(landing)
            .and_then(|id| self.materials.get(id))
            .is_some_and(|d| d.anchor);
        if !anchor {
            return;
        }
        let layer = LayerId::Substance(c);
        let have = ctx.world.layer(landing, layer);
        let added = amount.min(i32::from(u8::MAX) - have).max(0);
        if added > 0 {
            ctx.world.eco_put_layer(landing, layer, have + added);
            ctx.ledger.litter_in[usize::from(c)] += i64::from(added);
        }
    }

    /// Drops the fruit at `cell` of a plant of `species` (step 1): a part
    /// cell, which becomes air, when `part`; else a graph tree's fruit bud
    /// hanging in `cell`.
    fn fall(
        &self,
        ctx: &mut SweepCtx<'_>,
        form: &Form,
        species: u8,
        take: u8,
        cell: CellPos,
        part: bool,
    ) {
        let landing = self.landing(ctx.world, cell);
        let start = landing.map(above).filter(|&target| {
            let r = roll(
                self.seed ^ u64::from(self.season),
                b"take",
                cell.0.to_array(),
            );
            let world = &*ctx.world;
            r < take
                && world.get_cell(target) == Some(self.air)
                && world.get_cell(below(target)).is_some_and(|anchor| {
                    self.starts
                        .passes(species, target, anchor, |c, id| world.layer(c, id))
                })
        });
        if part {
            ctx.world.eco_set(cell, self.air, ctx.ledger);
        }
        match start {
            Some(target) => {
                ctx.world.eco_set(target, form.seed, ctx.ledger);
                ctx.world.eco_put_plant(
                    target,
                    Some(PlantState {
                        species,
                        ..PlantState::default()
                    }),
                );
                ctx.ledger.plants_born += 1;
            }
            None => self.rot(ctx, landing),
        }
    }
}

impl SweepPass for ExtendPass {
    fn kind(&self) -> usize {
        SWEEP_EXTEND
    }

    fn visit(&mut self, ctx: &mut SweepCtx<'_>, cell: CellPos) {
        let Some((species, form)) = self.form_at(ctx.world, cell).map(|(s, f)| (s, f.clone()))
        else {
            return;
        };
        let bare = form.shed == Some(self.row);
        let blooms = form
            .flower
            .as_ref()
            .is_some_and(|f| f.rows.contains(&self.row));
        let mut parts = parts_of(ctx.world, cell);

        // Falling fruit, wilting flowers, then a bare plant's leaves,
        // children first.
        let falls = form.fruit.filter(|f| f.fall == self.row);
        let wilts = !blooms && parts.iter().any(|(_, k)| *k == PartKind::Flower);
        if falls.is_some() || bare || wilts {
            for &(c, kind) in parts.iter().rev() {
                let is_part = Part::from_value(ctx.world.layer(c, LayerId::Part))
                    .is_some_and(|p| p.kind == kind);
                let drops = match kind {
                    PartKind::Flower => bare || !blooms,
                    PartKind::Fruit | PartKind::Leaf => bare,
                    _ => false,
                };
                match (kind, falls) {
                    (PartKind::Fruit, Some(f)) if is_part => {
                        self.fall(ctx, &form, species, f.take, c, true);
                    }
                    _ if is_part && drops => {
                        let landing = self.landing(ctx.world, c);
                        ctx.world.eco_set(c, self.air, ctx.ledger);
                        self.rot(ctx, landing);
                    }
                    _ => {}
                }
            }
            parts = parts_of(ctx.world, cell);
        }
        if let Shape::Graph(_) = &form.shape
            && !parts.is_empty()
        {
            for &(c, _) in parts.iter().rev() {
                if ctx.world.layer(c, LayerId::Part) != 0 {
                    let landing = self.landing(ctx.world, c);
                    ctx.world.eco_set(c, self.air, ctx.ledger);
                    self.rot(ctx, landing);
                }
            }
            parts.clear();
        }

        if let Shape::Graph(_) = &form.shape
            && let Some(graph) = ctx.world.tree(cell)
        {
            let (kept, dropped) = trees::drop_buds(graph, |bud| match bud {
                BudState::Fruit => bare || falls.is_some(),
                BudState::Flower => bare || !blooms,
                _ => false,
            });
            if !dropped.is_empty() {
                ctx.world.eco_put_tree(cell, Some(kept));
            }
            for (bud, at) in dropped {
                let from = trees::cell_at(cell, at);
                match (bud, falls) {
                    (BudState::Fruit, Some(f)) => {
                        self.fall(ctx, &form, species, f.take, from, false);
                    }
                    _ => {
                        let landing = self.landing(ctx.world, from);
                        self.rot(ctx, landing);
                    }
                }
            }
        }

        let mut graph = ctx.world.tree(cell).cloned().unwrap_or_default();
        if let Shape::Graph(growth) = &form.shape
            && !graph.is_empty()
        {
            let born = u16::try_from(self.season).unwrap_or(u16::MAX);
            let (tended, fallen) = {
                let site = self.site(ctx.world, cell);
                trees::tend(&graph, &site, growth, bare, born)
            };
            for at in fallen {
                let landing = self.landing(ctx.world, trees::cell_at(cell, at));
                self.rot_by(ctx, landing, self.metamer_litter);
            }
            ctx.world.eco_put_tree(cell, Some(tended.clone()));
            graph = tended;
        }
        let lit = parts
            .iter()
            .filter(|&&(c, kind)| {
                Form::feeds(kind) && ctx.world.layer(c, LayerId::Light) >= form.leaf_light
            })
            .count()
            + trees::lit_leaves(&graph, &self.site(ctx.world, cell), form.leaf_light);
        let lit = i32::try_from(lit).unwrap_or(i32::MAX);
        let fed = lit.saturating_mul(form.leaf_feed) / FEED_DIV;
        let mut biomass = (ctx.world.layer(cell, LayerId::Biomass) + fed).min(u8::MAX.into());
        if !bare && biomass >= form.cost {
            if ctx.world.get_cell(cell) == Some(form.seed) {
                ctx.world.eco_set(cell, form.wood, ctx.ledger);
                biomass -= form.cost;
            } else if let Shape::Graph(growth) = &form.shape {
                let mut budget = usize::try_from(biomass / form.cost).unwrap_or(0);
                let born = u16::try_from(self.season).unwrap_or(u16::MAX);
                if graph.len() >= growth.mature {
                    let flowers = form
                        .flower
                        .as_ref()
                        .filter(|_| blooms)
                        .map(|f| (BudState::Flower, f.count, form.leaf_light));
                    let fruit = form
                        .fruit
                        .filter(|f| f.grow == self.row)
                        .map(|f| (BudState::Fruit, f.count, 0));
                    for (bud, most, light) in [flowers, fruit].into_iter().flatten() {
                        let want = most.saturating_sub(trees::count(&graph, bud)).min(budget);
                        let (bearing, set) = {
                            let site = self.site(ctx.world, cell);
                            let seed = self.seed ^ u64::from(self.season);
                            trees::bear(&graph, &site, bud, want, light, seed)
                        };
                        if set > 0 {
                            graph = bearing;
                            budget -= set;
                            biomass -= i32::try_from(set).unwrap_or(i32::MAX) * form.cost;
                        }
                    }
                }
                let (grown, added) = {
                    let site = self.site(ctx.world, cell);
                    trees::grow(&graph, &site, growth, budget, born, self.seed)
                };
                ctx.world.eco_put_tree(cell, Some(grown));
                biomass -= i32::try_from(added).unwrap_or(i32::MAX) * form.cost;
            } else {
                let mut members: BTreeMap<CellPos, Option<PartKind>> =
                    parts.iter().map(|&(c, k)| (c, Some(k))).collect();
                members.insert(cell, None);
                let count = |kind| parts.iter().filter(|(_, k)| *k == kind).count();
                let fruiting = form
                    .fruit
                    .is_some_and(|f| f.grow == self.row && count(PartKind::Fruit) < f.count);
                let flowering = blooms
                    && form
                        .flower
                        .as_ref()
                        .is_some_and(|f| count(PartKind::Flower) < f.count);
                let world = &*ctx.world;
                let lit = |c: CellPos| world.layer(c, LayerId::Light) >= form.leaf_light;
                let flower = || {
                    flowering
                        .then(|| self.next_beside(world, cell, &members, b"flower", lit))
                        .flatten()
                        .map(|(at, leaf)| (at, leaf, PartKind::Flower))
                };
                let fruit = || {
                    fruiting
                        .then(|| self.next_beside(world, cell, &members, b"fruit", |_| true))
                        .flatten()
                        .map(|(at, leaf)| (at, leaf, PartKind::Fruit))
                };
                // Flowers and fruit wait for the full form, flowers first.
                let grow = self
                    .next_part(world, &form, cell, &members)
                    .or_else(flower)
                    .or_else(fruit);
                if let Some((at, parent, kind)) = grow {
                    let part = Part::toward(at, parent, kind).expect("a face neighbor");
                    ctx.world.eco_set(at, form.material(kind), ctx.ledger);
                    ctx.world
                        .eco_put_layer(at, LayerId::Part, i32::from(part.value()));
                    biomass -= form.cost;
                }
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

/// The 18 cells sharing a face or an edge with `c`, in a fixed order.
fn touching(c: CellPos) -> impl Iterator<Item = CellPos> {
    (-1..=1).flat_map(move |y: i32| {
        (-1..=1).flat_map(move |z: i32| {
            (-1..=1)
                .filter(move |x: &i32| {
                    let d = x.abs() + y.abs() + z.abs();
                    d == 1 || d == 2
                })
                .map(move |x| CellPos::new(c.0.x + x, c.0.y + y, c.0.z + z))
        })
    })
}

fn above(c: CellPos) -> CellPos {
    CellPos::new(c.0.x, c.0.y + 1, c.0.z)
}

fn below(c: CellPos) -> CellPos {
    CellPos::new(c.0.x, c.0.y - 1, c.0.z)
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

    use crate::tables::fixtures::cell_tree_sources;

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
    use crate::tick::season_tick_observed;
    use crate::water::WaterPass;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    const FED: [(&str, i32); 3] = [("n", 50), ("p", 50), ("k", 50)];
    const SEED: u64 = 7;

    /// The shipped tables with the trees back on their cell forms
    /// (docs/TREES.md T7: the shipped trees grow graphs).
    fn tables() -> EcoTables {
        let shipped = EcoTables::load(ASSETS).expect("Phase 2 tables load");
        EcoTables::from_sources(Arc::clone(shipped.materials()), &cell_tree_sources(ASSETS))
            .expect("the cell trees load")
    }

    /// The shipped tables with the oak's trunk height fixed at `h`.
    fn tables_with_trunk(h: u8) -> EcoTables {
        let shipped = tables();
        let mut src = cell_tree_sources(ASSETS);
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
        let FormDef::Tree { trunk, crown } = oak.parts.as_ref().unwrap().form else {
            panic!("oak is a tree");
        };
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
        // Every cell of the crown is a leaf or a branch, and every one in
        // the crown; the branches (docs/VEGETATION.md V5) are the center
        // and the cells on its horizontal axes short of the radius.
        let top = height - 1;
        let in_crown = |[x, y, z]: [i32; 3]| {
            let dy = y - top - 1;
            (x * x + z * z) * h * h + dy * dy * r * r <= r * r * h * h
        };
        let leaves: Vec<[i32; 3]> = tree
            .iter()
            .filter(|&(_, &k)| matches!(k, PartKind::Leaf | PartKind::Branch))
            .map(|(&o, _)| o)
            .collect();
        assert!(leaves.iter().all(|&o| in_crown(o)));
        for &[x, y, z] in &leaves {
            let axis = y == top + 1 && (x == 0 || z == 0) && x.abs().max(z.abs()) < r;
            let want = if axis {
                PartKind::Branch
            } else {
                PartKind::Leaf
            };
            assert_eq!(tree[&[x, y, z]], want, "{:?}", [x, y, z]);
        }
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

    /// The shipped tables with `from` replaced by `to` in `plants.ron`
    /// (once: `from` must name one row's text).
    fn tables_with(from: &str, to: &str) -> EcoTables {
        let shipped = tables();
        let mut src = cell_tree_sources(ASSETS);
        assert_eq!(src.plants.matches(from).count(), 1, "{from}");
        src.plants = src.plants.replace(from, to);
        EcoTables::from_sources(Arc::clone(shipped.materials()), &src).expect("edited rows load")
    }

    /// One extension sweep in season `season`, checking the conservation
    /// invariants and the parts invariant.
    fn extend_in(w: &mut World, t: &EcoTables, season: u32) -> SeasonLedger {
        let checker = InvariantChecker::new(w);
        let mut ledger = SeasonLedger::default();
        ExtendPass::for_season(t, season, SEED).run(w, &mut ledger);
        checker.check(w, &ledger).unwrap_or_else(|v| panic!("{v}"));
        check_parts(w).unwrap();
        ledger
    }

    /// The season index of `key` in the first year.
    fn season(t: &EcoTables, key: &str) -> u32 {
        t.seasons()
            .seasons
            .iter()
            .position(|s| s.key == key)
            .unwrap() as u32
    }

    /// The loam under `x0..=x1, z0..=z1` damp and fed, so seeds can start.
    fn feed_soil(w: &mut World, t: &EcoTables, (x0, x1): (i32, i32), (z0, z1): (i32, i32)) {
        for z in z0..=z1 {
            for x in x0..=x1 {
                let anchor = CellPos::new(x, 30, z);
                w.eco_put_layer(anchor, LayerId::Moisture, 100);
                for (k, n) in FED {
                    let s = t.substance_index(k).unwrap();
                    w.eco_put_layer(anchor, LayerId::Substance(s), n);
                }
            }
        }
    }

    fn brush(w: &mut World, seq: u32, op: EditOp, cell: CellPos) -> rmc_world::edit::EditResult {
        let issuer = PlayerId(1);
        w.apply(&EditCommand {
            id: CommandId { issuer, seq },
            issuer,
            op,
            shape: Shape::Cell(cell),
        })
    }

    fn kinds(w: &World, root: CellPos, kind: PartKind) -> Vec<CellPos> {
        parts_of(w, root)
            .into_iter()
            .filter(|&(_, k)| k == kind)
            .map(|(c, _)| c)
            .collect()
    }

    #[test]
    fn a_seed_grows_as_a_seedling_then_ripens_into_its_wood() {
        let t = tables();
        let cost = i32::from(
            t.plants()[usize::from(species(&t, "oak"))]
                .parts
                .as_ref()
                .unwrap()
                .cost,
        );
        let mut w = world(&t);
        let root = CellPos::new(10, 31, 10);
        feed_soil(&mut w, &t, (10, 10), (10, 10));

        // Placing wood plants nothing; placing the seed plants an oak.
        let r = brush(&mut w, 1, EditOp::Set(id(&t, "oak_wood")), root);
        assert_eq!(r.status, EditStatus::Applied);
        assert!(w.plant(root).is_none(), "wood is no seed");
        brush(&mut w, 2, EditOp::Set(MaterialId::AIR), root);
        brush(&mut w, 3, EditOp::Set(id(&t, "acorn")), root);
        let plant = *w.plant(root).expect("the acorn starts a plant");
        assert_eq!(plant.species, species(&t, "oak"));

        // A seedling short of the cost stays a seedling.
        w.eco_put_layer(root, LayerId::Growth, 255);
        w.eco_put_layer(root, LayerId::Biomass, cost - 1);
        extend(&mut w, &t);
        assert_eq!(w.get_cell(root), Some(id(&t, "acorn")));
        // At the cost it ripens: the cell is wood, the plant the same, the
        // growth kept and the cost paid; no part grows that step.
        w.eco_put_layer(root, LayerId::Biomass, cost + 2);
        extend(&mut w, &t);
        assert_eq!(w.get_cell(root), Some(id(&t, "oak_wood")));
        assert_eq!(w.plant(root), Some(&plant));
        assert_eq!(w.layer(root, LayerId::Growth), 255);
        assert_eq!(w.layer(root, LayerId::Biomass), 2);
        assert!(parts_of(&w, root).is_empty());
        // Then the trunk grows from the ripened root.
        w.eco_put_layer(root, LayerId::Biomass, cost);
        extend(&mut w, &t);
        assert_eq!(kinds(&w, root, PartKind::Trunk), [above(root)]);
    }

    #[test]
    fn a_bare_tree_sheds_its_leaves_as_litter_and_regrows_them() {
        let t = tables();
        let litter = i32::from(t.params().part_litter.unwrap());
        let organic = LayerId::Substance(t.role_substance("organic").unwrap());
        let winter = season(&t, "winter");
        let mut w = world(&t);
        let root = CellPos::new(10, 31, 10);
        oak(&mut w, &t, root, 255);
        grow_out(&mut w, &t, root);
        let tree = shape(&w, root);
        let leaves = kinds(&w, root, PartKind::Leaf);
        assert!(!leaves.is_empty() && !kinds(&w, root, PartKind::Branch).is_empty());
        let soil_organic = |w: &World| -> i32 {
            (0..32)
                .flat_map(|z| (0..32).map(move |x| CellPos::new(x, 30, z)))
                .map(|c| w.layer(c, organic))
                .sum()
        };
        let had = soil_organic(&w);

        // Winter: every leaf falls to the soil below it as litter; the
        // wood stays, and nothing grows, whatever the biomass.
        w.eco_put_layer(root, LayerId::Biomass, 255);
        let ledger = extend_in(&mut w, &t, winter);
        for &c in &leaves {
            assert_eq!(w.get_cell(c), Some(MaterialId::AIR), "{c:?}");
        }
        let bare = shape(&w, root);
        assert!(bare.values().all(|&k| k != PartKind::Leaf));
        assert_eq!(
            bare.len(),
            tree.len() - leaves.len(),
            "only the leaves fell"
        );
        let fallen = leaves.len() as i32 * litter;
        assert_eq!(soil_organic(&w) - had, fallen);
        assert_eq!(
            ledger.litter_in[usize::from(t.role_substance("organic").unwrap())],
            i64::from(fallen)
        );
        assert_eq!(
            w.layer(root, LayerId::Biomass),
            255,
            "a bare tree spends nothing"
        );
        extend_in(&mut w, &t, winter);
        assert_eq!(shape(&w, root), bare);

        // Spring: the same crown grows back.
        grow_out(&mut w, &t, root);
        assert_eq!(shape(&w, root), tree);
    }

    #[test]
    fn a_full_crown_fruits_in_its_season_and_fallen_fruit_takes_root_or_rots() {
        for (take, rooted) in [(0, false), (255, true)] {
            let t = tables_with("take: 48", &format!("take: {take}"));
            let oak_row = &t.plants()[usize::from(species(&t, "oak"))];
            let count = usize::from(
                oak_row
                    .parts
                    .as_ref()
                    .unwrap()
                    .fruit
                    .as_ref()
                    .unwrap()
                    .count,
            );
            let (autumn, winter) = (season(&t, "autumn"), season(&t, "winter"));
            let organic = t.role_substance("organic").unwrap();
            let mut w = world(&t);
            let root = CellPos::new(10, 31, 10);
            oak(&mut w, &t, root, 255);
            feed_soil(&mut w, &t, (4, 16), (4, 16));
            grow_out(&mut w, &t, root);
            let tree = shape(&w, root);

            // No fruit outside its season; in it, up to `count`, one a
            // step, each hanging from a leaf, made of the seed.
            w.eco_put_layer(root, LayerId::Biomass, 255);
            extend(&mut w, &t);
            assert!(kinds(&w, root, PartKind::Fruit).is_empty());
            for step in 1..=count + 2 {
                w.eco_put_layer(root, LayerId::Biomass, 255);
                extend_in(&mut w, &t, autumn);
                assert_eq!(kinds(&w, root, PartKind::Fruit).len(), step.min(count));
            }
            let fruit = kinds(&w, root, PartKind::Fruit);
            for &f in &fruit {
                assert_eq!(w.get_cell(f), Some(id(&t, "acorn")));
                let part = Part::from_value(w.layer(f, LayerId::Part)).unwrap();
                assert_eq!(
                    tree.get(&(part.parent(f).0 - root.0).to_array()),
                    Some(&PartKind::Leaf)
                );
                assert!(w.plant(f).is_none(), "fruit on the tree is no plant");
            }

            // Winter: the fruit falls. With take 0 it all rots; with 255
            // some takes root as a seedling on the soil below it.
            let ledger = extend_in(&mut w, &t, winter);
            assert!(kinds(&w, root, PartKind::Fruit).is_empty());
            for &f in &fruit {
                assert_eq!(w.get_cell(f), Some(MaterialId::AIR));
            }
            let born: Vec<CellPos> = (0..32)
                .flat_map(|z| (0..32).map(move |x| CellPos::new(x, 31, z)))
                .filter(|&c| c != root && w.plant(c).is_some())
                .collect();
            assert_eq!(born.len() as u32, ledger.plants_born);
            assert_eq!(!born.is_empty(), rooted, "take {take}: {born:?}");
            for &c in &born {
                assert_eq!(w.get_cell(c), Some(id(&t, "acorn")));
                assert_eq!(w.plant(c).unwrap().species, species(&t, "oak"));
            }
            let rotted = ledger.litter_in[usize::from(organic)];
            if !rooted {
                assert!(rotted > 0, "fallen fruit rots on the soil");
            }
        }
    }

    #[test]
    fn wood_leaves_and_fruit_dig_as_items_and_the_seed_plants() {
        let t = tables();
        let mut w = world(&t);
        let root = CellPos::new(10, 31, 10);
        oak(&mut w, &t, root, 255);
        grow_out(&mut w, &t, root);
        for _ in 0..3 {
            w.eco_put_layer(root, LayerId::Biomass, 255);
            extend_in(&mut w, &t, season(&t, "autumn"));
        }
        let fruit = kinds(&w, root, PartKind::Fruit)[0];
        let leaf = *kinds(&w, root, PartKind::Leaf).last().unwrap();
        let trunk = kinds(&w, root, PartKind::Trunk)[0];
        let mut seq = 0;
        let mut dig = |w: &mut World, c: CellPos| {
            seq += 1;
            let r = brush(w, seq, EditOp::Damage(u16::MAX), c);
            assert_eq!(r.status, EditStatus::Applied);
            r.yields
        };
        assert_eq!(dig(&mut w, fruit), [(id(&t, "acorn"), 1)]);
        assert_eq!(dig(&mut w, leaf), [(id(&t, "oak_leaves"), 1)]);
        assert_eq!(dig(&mut w, trunk), [(id(&t, "oak_wood"), 1)]);
        check_parts(&w).unwrap();

        // The dug seed plants an oak where it is placed on soil.
        let spot = CellPos::new(20, 31, 20);
        brush(&mut w, 100, EditOp::Set(id(&t, "acorn")), spot);
        assert_eq!(w.plant(spot).map(|p| p.species), Some(species(&t, "oak")));
    }

    #[test]
    fn tree_rows_name_known_seasons_and_bear_fruit() {
        let shipped = tables();
        let try_load = |from: &str, to: &str| {
            let mut src = cell_tree_sources(ASSETS);
            assert_eq!(src.plants.matches(from).count(), 1, "{from}");
            src.plants = src.plants.replace(from, to);
            EcoTables::from_sources(Arc::clone(shipped.materials()), &src)
        };
        assert!(try_load("take: 48", "take: 0").is_ok());
        for (from, to) in [
            ("fall: \"winter\"", "fall: \"monsoon\""),
            ("grow: \"autumn\"", "grow: \"winter\""),
            ("count: 6", "count: 0"),
        ] {
            assert!(try_load(from, to).is_err(), "{to}");
        }
        let unknown_shed = {
            let mut src = cell_tree_sources(ASSETS);
            src.plants = src
                .plants
                .replacen("shed: Some(\"winter\")", "shed: Some(\"dusk\")", 1);
            EcoTables::from_sources(Arc::clone(shipped.materials()), &src)
        };
        assert!(unknown_shed.is_err());
    }

    #[test]
    fn a_seed_grows_into_a_tree_that_stands_bare_in_winter() {
        let t = tables();
        let mut w = world(&t);
        let root = CellPos::new(10, 31, 10);
        feed_soil(&mut w, &t, (10, 10), (10, 10));
        brush(&mut w, 1, EditOp::Set(id(&t, "acorn")), root);
        w.eco_put_layer(root, LayerId::Temperature, 200);
        activate_holders(&mut w, &WaterPass::new(&t));
        let mut activity = Activity::new(t.params().quiet_skip);
        for season in 0..4 {
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
            if season == 2 {
                assert!(
                    !kinds(&w, root, PartKind::Leaf).is_empty(),
                    "leaves by autumn"
                );
            }
        }
        assert!(w.plant(root).is_some(), "the oak lives");
        assert_eq!(w.get_cell(root), Some(id(&t, "oak_wood")), "ripened");
        assert!(!kinds(&w, root, PartKind::Trunk).is_empty());
        assert!(kinds(&w, root, PartKind::Leaf).is_empty(), "bare in winter");
    }

    /// A vine of `key` rooted at `cell` (y 31): its stem with a grown plant
    /// state, 20.0 °C, on fed, moist loam.
    fn vine(w: &mut World, t: &EcoTables, key: &str, cell: CellPos) {
        let mut scratch = SeasonLedger::default();
        w.eco_set(cell, id(t, "vine_stem"), &mut scratch);
        w.eco_put_plant(
            cell,
            Some(PlantState {
                species: species(t, key),
                ..PlantState::default()
            }),
        );
        w.eco_put_layer(cell, LayerId::Growth, 255);
        w.eco_put_layer(cell, LayerId::Temperature, 200);
        feed_soil(w, t, (cell.0.x, cell.0.x), (cell.0.z, cell.0.z));
    }

    /// Stone at every cell of `x.0..=x.1, y.0..=y.1, z.0..=z.1`.
    fn stone(w: &mut World, t: &EcoTables, x: (i32, i32), y: (i32, i32), z: (i32, i32)) {
        let mut scratch = SeasonLedger::default();
        for cz in z.0..=z.1 {
            for cy in y.0..=y.1 {
                for cx in x.0..=x.1 {
                    w.eco_set(CellPos::new(cx, cy, cz), id(t, "stone"), &mut scratch);
                }
            }
        }
    }

    /// Extension sweeps in season `season` with the root's biomass topped
    /// up each time, until the plant stops growing.
    fn grow_out_in(w: &mut World, t: &EcoTables, root: CellPos, season: u32) {
        for _ in 0..400 {
            let before = parts_of(w, root).len();
            w.eco_put_layer(root, LayerId::Biomass, 255);
            extend_in(w, t, season);
            if parts_of(w, root).len() == before {
                return;
            }
        }
        panic!("the plant stops growing");
    }

    /// Whether `c` touches stone along a face or an edge.
    fn on_stone(w: &World, t: &EcoTables, c: CellPos) -> bool {
        touching(c).any(|n| w.get_cell(n) == Some(id(t, "stone")))
    }

    #[test]
    fn a_vine_climbs_a_wall_up_first_and_stops_at_its_reach() {
        let t = tables();
        let reach = 24;
        let mut w = world(&t);
        stone(&mut w, &t, (12, 12), (31, 40), (8, 12));
        let root = CellPos::new(11, 31, 10);
        vine(&mut w, &t, "ivy", root);

        w.eco_put_layer(root, LayerId::Biomass, 255);
        extend(&mut w, &t);
        assert_eq!(kinds(&w, root, PartKind::Vine), [above(root)]);
        // Up the face of the wall, and over its edge, before anything
        // sideways.
        for _ in 0..9 {
            w.eco_put_layer(root, LayerId::Biomass, 255);
            extend(&mut w, &t);
        }
        let mut grown = kinds(&w, root, PartKind::Vine);
        grown.sort();
        let column: Vec<CellPos> = (32..=41).map(|y| CellPos::new(11, y, 10)).collect();
        assert_eq!(grown, column, "it climbs the face and crests the wall");

        grow_out_in(&mut w, &t, root, 0);
        let vines = kinds(&w, root, PartKind::Vine);
        assert_eq!(vines.len(), reach);
        for &c in &vines {
            assert_eq!(w.get_cell(c), Some(id(&t, "ivy_leaves")));
            assert!(on_stone(&w, &t, c), "{c:?} clings to the wall");
        }
    }

    #[test]
    fn a_vine_without_support_stays_a_stem() {
        let t = tables();
        let mut w = world(&t);
        let root = CellPos::new(11, 31, 10);
        vine(&mut w, &t, "ivy", root);
        for _ in 0..4 {
            w.eco_put_layer(root, LayerId::Biomass, 255);
            extend(&mut w, &t);
        }
        assert!(parts_of(&w, root).is_empty());
        assert_eq!(w.layer(root, LayerId::Biomass), 255, "nothing spent");
    }

    #[test]
    fn a_vine_hangs_over_a_ledge_only_when_its_row_says_so() {
        for hang in [false, true] {
            let t = tables_with(
                "reach: 24, hang: true",
                &format!("reach: 255, hang: {hang}"),
            );
            let mut w = world(&t);
            // A pillar with a beam on top, reaching out over open ground.
            stone(&mut w, &t, (20, 20), (31, 33), (10, 10));
            stone(&mut w, &t, (21, 23), (33, 33), (10, 10));
            let root = CellPos::new(19, 31, 10);
            vine(&mut w, &t, "ivy", root);
            grow_out_in(&mut w, &t, root, 0);
            let vines = kinds(&w, root, PartKind::Vine);
            assert!(vines.len() > 8, "it covers the stone: {}", vines.len());
            let hanging: Vec<CellPos> = vines
                .iter()
                .copied()
                .filter(|&c| !on_stone(&w, &t, c))
                .collect();
            if hang {
                assert!(!hanging.is_empty(), "a curtain below the beam");
                for &c in &hanging {
                    let part = Part::from_value(w.layer(c, LayerId::Part)).unwrap();
                    assert_eq!(part.parent(c), above(c), "{c:?} hangs straight down");
                }
            } else {
                assert!(hanging.is_empty(), "{hanging:?}");
            }
        }
    }

    #[test]
    fn vines_flower_in_their_season_and_wilt_after() {
        let t = tables();
        let (spring, autumn) = (season(&t, "spring"), season(&t, "autumn"));
        let count = 5;
        let mut w = world(&t);
        stone(&mut w, &t, (12, 12), (31, 40), (8, 12));
        let root = CellPos::new(11, 31, 10);
        vine(&mut w, &t, "clematis", root);

        // In the dark no flower opens; the vine grows out.
        grow_out_in(&mut w, &t, root, spring);
        assert!(kinds(&w, root, PartKind::Flower).is_empty());
        let vines = kinds(&w, root, PartKind::Vine);
        assert!(!vines.is_empty());

        // Lit, it opens a flower a step, up to `count`, beside its vine
        // cells.
        for &c in &vines {
            w.eco_put_layer(c, LayerId::Light, 200);
        }
        for step in 1..=count + 2 {
            w.eco_put_layer(root, LayerId::Biomass, 255);
            extend_in(&mut w, &t, spring);
            assert_eq!(kinds(&w, root, PartKind::Flower).len(), step.min(count));
        }
        let flowers = kinds(&w, root, PartKind::Flower);
        for &f in &flowers {
            assert_eq!(w.get_cell(f), Some(id(&t, "clematis_flower")));
            let part = Part::from_value(w.layer(f, LayerId::Part)).unwrap();
            assert!(vines.contains(&part.parent(f)), "{f:?} opens on the vine");
        }

        // Autumn: the flowers wilt; the vine stays.
        extend_in(&mut w, &t, autumn);
        assert!(kinds(&w, root, PartKind::Flower).is_empty());
        for &f in &flowers {
            assert_eq!(w.get_cell(f), Some(MaterialId::AIR));
        }
        assert_eq!(kinds(&w, root, PartKind::Vine).len(), vines.len());
    }

    #[test]
    fn vine_rows_reject_empty_rules_and_unknown_flower_seasons() {
        let shipped = tables();
        let try_load = |from: &str, to: &str| {
            let mut src = cell_tree_sources(ASSETS);
            assert_eq!(src.plants.matches(from).count(), 1, "{from}");
            src.plants = src.plants.replace(from, to);
            EcoTables::from_sources(Arc::clone(shipped.materials()), &src)
        };
        assert!(try_load("reach: 24, hang: true", "reach: 24, hang: false").is_ok());
        for (from, to) in [
            ("reach: 24, hang: true", "reach: 0, hang: true"),
            (
                "climb_tags: [\"stone\", \"wood\"], reach: 24",
                "climb_tags: [], reach: 24",
            ),
            (
                "seasons: [\"spring\", \"summer\"]",
                "seasons: [\"spring\", \"monsoon\"]",
            ),
            ("seasons: [\"spring\", \"summer\"]", "seasons: []"),
            ("count: 5))", "count: 0))"),
        ] {
            assert!(try_load(from, to).is_err(), "{to}");
        }
        // An unknown flower material is listed like any unknown material.
        let lily = try_load("material: \"clematis_flower\"", "material: \"lily\"").unwrap();
        assert!(lily.unresolved_materials().contains("lily"));
    }

    #[test]
    fn an_ivy_seed_by_a_wall_grows_up_it_through_the_seasons() {
        let t = tables();
        let mut w = world(&t);
        stone(&mut w, &t, (12, 12), (31, 40), (8, 12));
        let root = CellPos::new(11, 31, 10);
        feed_soil(&mut w, &t, (11, 11), (10, 10));
        brush(&mut w, 1, EditOp::Set(id(&t, "ivy_seed")), root);
        assert_eq!(w.plant(root).map(|p| p.species), Some(species(&t, "ivy")));
        w.eco_put_layer(root, LayerId::Temperature, 200);
        activate_holders(&mut w, &WaterPass::new(&t));
        let mut activity = Activity::new(t.params().quiet_skip);
        for season in 0..4 {
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
        assert!(w.plant(root).is_some(), "the ivy lives");
        assert_eq!(w.get_cell(root), Some(id(&t, "vine_stem")), "ripened");
        let vines = kinds(&w, root, PartKind::Vine);
        assert!(!vines.is_empty(), "evergreen ivy holds its vine in winter");
        assert!(vines.contains(&above(root)));
    }
}
