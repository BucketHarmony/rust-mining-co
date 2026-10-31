//! Tree growth (docs/TREES.md §4.2, T2): a tree of a `form: Graph(...)`
//! row grows its metamer graph by a light-driven bud model (Palubicki et
//! al. 2009), in integers (rule 3).
//!
//! A graph's buds live on its metamers: a metamer without a continuation
//! (a tip) carries its terminal bud in `bud`; any other carries its
//! axillary bud there, Active until it sprouts a lateral axis (then Dead)
//! or is blocked (then Dormant).
//!
//! **One flush**, from the previous state:
//! 1. **Light.** An Active bud sees the light of the cell its metamer ends
//!    in, or nothing below `bud_light`. `Q` sums the buds' light base-ward
//!    over every subtree.
//! 2. **Vigor.** The flush can pay for `budget` metamers; `budget · 256`
//!    vigor (Q8) enters the first metamer and splits at every node by
//!    Borchert–Honda: the main side (the continuation, or a tip's terminal
//!    bud) gets `v · λ·Qm / (λ·Qm + (256−λ)·Ql)`, the lateral side (the
//!    lateral axis, or the axillary bud) the rest.
//! 3. **Shoots.** A bud with vigor `v` adds `min(v / 256, max_shoot)`
//!    metamers of `internode_mm`; what the budget has left goes one metamer
//!    each to the buds with the largest fractions of `v / 256` (ties to the
//!    lower index), so a crown of many weak buds still grows. Buds grow in
//!    metamer order. A lateral leaves
//!    its axis at `branch_angle`, turned about the axis by `divergence`
//!    per node along it plus a seeded phase per axis. Every later internode
//!    leans from the one before toward up (`gravitropism`) and up the
//!    light gradient of the cell it starts in (`phototropism`).
//! 4. **Collision.** An internode whose end cell is not passable (open,
//!    a part, or a plant) turns once toward the brightest passable face
//!    of the cell it starts in; still blocked, its bud goes Dormant.
//! 5. **Cap.** No metamer is added past `max_metamers`.
//!
//! A tree reads only the light layer and the cells it would grow into, and
//! writes only its own graph and its root's biomass, so trees grow the
//! same in any order. Rolls are keyed by the world seed and metamer
//! indices, never by where the tree stands, so a tree grows the same
//! anywhere with the same light around it.

use rmc_world::coords::{CELL_SIZE_MM, CellPos};
use rmc_world::tree::{
    self, BudState, Dir, Metamer, NO_PARENT, Q14, TURN, TreeGraph, normalize, rotate,
};
use xxhash_rust::xxh3::Xxh3;

use crate::tables::FormDef;

/// The Q8 fraction bits of vigor and of λ.
const Q8_BITS: u32 = 8;
/// 1.0 in Q8.
const Q8: i64 = 1 << Q8_BITS;
/// Straight up.
const UP: Dir = [0, Q14 as i16, 0];
/// The six face directions, in a fixed order.
const FACES: [[i32; 3]; 6] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

/// A `form: Graph(...)` row, widened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Growth {
    pub internode_mm: u16,
    pub tip_um: u32,
    pub branch_angle: u32,
    pub divergence: u32,
    pub lambda: i64,
    pub gravitropism: i64,
    pub phototropism: i64,
    pub bud_light: i32,
    pub max_shoot: usize,
    pub max_metamers: usize,
    pub leaf: u8,
}

impl Growth {
    /// The growth rule of a `Graph` form; `None` for any other form.
    pub fn from_form(form: &FormDef) -> Option<Self> {
        let &FormDef::Graph {
            internode_mm,
            tip_um,
            branch_angle,
            divergence,
            lambda,
            gravitropism,
            phototropism,
            bud_light,
            max_shoot,
            max_metamers,
            leaf,
        } = form
        else {
            return None;
        };
        Some(Self {
            internode_mm,
            tip_um,
            branch_angle: u32::from(branch_angle),
            divergence: u32::from(divergence),
            lambda: i64::from(lambda),
            gravitropism: i64::from(gravitropism),
            phototropism: i64::from(phototropism),
            bud_light: i32::from(bud_light),
            max_shoot: usize::from(max_shoot),
            max_metamers: usize::from(max_metamers),
            leaf,
        })
    }
}

/// What a tree reads of the world around it.
pub struct Site<L, P> {
    /// The root cell.
    pub root: CellPos,
    /// The light layer at a cell.
    pub light: L,
    /// Whether an internode may end in a cell.
    pub passable: P,
}

impl<L: Fn(CellPos) -> i32, P: Fn(CellPos) -> bool> Site<L, P> {
    /// The cell holding the point `at`, in millimeters from the root
    /// cell's top-face center.
    pub fn cell(&self, at: [i32; 3]) -> CellPos {
        let r = self.root.0;
        let half = CELL_SIZE_MM / 2;
        let mm = [
            r.x * CELL_SIZE_MM + half + at[0],
            (r.y + 1) * CELL_SIZE_MM + at[1],
            r.z * CELL_SIZE_MM + half + at[2],
        ];
        let [x, y, z] = mm.map(|c| c.div_euclid(CELL_SIZE_MM));
        CellPos::new(x, y, z)
    }

    fn light_at(&self, at: [i32; 3]) -> i32 {
        (self.light)(self.cell(at))
    }

    /// The light gradient at the cell holding `at`, as a unit vector;
    /// `None` when the light is level.
    fn gradient(&self, at: [i32; 3]) -> Option<Dir> {
        let c = self.cell(at).0;
        let l = |d: [i32; 3]| {
            i64::from((self.light)(CellPos::new(
                c.x + d[0],
                c.y + d[1],
                c.z + d[2],
            )))
        };
        let g: [i64; 3] = std::array::from_fn(|k| {
            let mut plus = [0; 3];
            plus[k] = 1;
            l(plus) - l(plus.map(|x| -x))
        });
        normalize(g)
    }

    /// The brightest passable face neighbor of the cell holding `at`, as a
    /// direction (the first in [`FACES`] order on a tie).
    fn brightest_face(&self, at: [i32; 3]) -> Option<Dir> {
        let c = self.cell(at).0;
        let mut best: Option<(i32, [i32; 3])> = None;
        for f in FACES {
            let n = CellPos::new(c.x + f[0], c.y + f[1], c.z + f[2]);
            if !(self.passable)(n) {
                continue;
            }
            let l = (self.light)(n);
            if best.is_none_or(|(b, _)| l > b) {
                best = Some((l, f));
            }
        }
        best.map(|(_, f)| f.map(|x| (x * Q14) as i16))
    }
}

/// How many of the graph's leafy metamers end in a cell with at least
/// `leaf_light`.
pub fn lit_leaves<L, P>(graph: &TreeGraph, site: &Site<L, P>, leaf_light: i32) -> usize
where
    L: Fn(CellPos) -> i32,
    P: Fn(CellPos) -> bool,
{
    graph
        .metamers()
        .iter()
        .zip(graph.ends())
        .filter(|(m, end)| m.leaf > 0 && site.light_at(*end) >= leaf_light)
        .count()
}

/// One flush of the tree `graph` at `site` that can pay for `budget`
/// metamers: the grown graph and how many metamers it added. An empty graph
/// grows its first internode straight up. `born` stamps the new metamers;
/// `seed` is the world seed.
pub fn grow<L, P>(
    graph: &TreeGraph,
    site: &Site<L, P>,
    g: &Growth,
    budget: usize,
    born: u16,
    seed: u64,
) -> (TreeGraph, usize)
where
    L: Fn(CellPos) -> i32,
    P: Fn(CellPos) -> bool,
{
    let mut out = graph.clone();
    let room = g.max_metamers.saturating_sub(graph.len());
    if budget == 0 || room == 0 {
        return (out, 0);
    }
    let new = |parent: u16, dir: Dir, lateral: bool| Metamer {
        parent,
        dir,
        length_mm: g.internode_mm,
        born,
        bud: BudState::Active,
        leaf: g.leaf,
        shade: 0,
        lateral,
    };
    if graph.is_empty() {
        let end = tree::step(UP, g.internode_mm);
        if !(site.passable)(site.cell(end)) {
            return (out, 0);
        }
        let added = usize::from(out.push(new(NO_PARENT, UP, false)).is_ok());
        return (out, added);
    }

    let ms = graph.metamers();
    let n = ms.len();
    let ends = graph.ends();
    let mut cont: Vec<Option<usize>> = vec![None; n];
    let mut lat: Vec<Option<usize>> = vec![None; n];
    let mut rank = vec![0u32; n];
    let mut axis = vec![0usize; n];
    for (i, m) in ms.iter().enumerate().skip(1) {
        let p = usize::from(m.parent);
        if m.lateral {
            lat[p] = Some(i);
            axis[i] = i;
        } else {
            cont[p] = Some(i);
            rank[i] = rank[p] + 1;
            axis[i] = axis[p];
        }
    }

    // 1. Light: each Active bud's, then Q base-ward.
    let q: Vec<i64> = (0..n)
        .map(|i| {
            let open = cont[i].is_none() || lat[i].is_none();
            if ms[i].bud != BudState::Active || !open {
                return 0;
            }
            let l = site.light_at(ends[i]);
            if l >= g.bud_light { i64::from(l) } else { 0 }
        })
        .collect();
    let mut big_q = q.clone();
    for i in (0..n).rev() {
        for c in [cont[i], lat[i]].into_iter().flatten() {
            big_q[i] += big_q[c];
        }
    }

    // 2. Vigor, split top-down.
    let lambda = g.lambda;
    let mut vigor = vec![0i64; n];
    let mut bud_vigor = vec![0i64; n];
    vigor[0] = i64::try_from(budget).unwrap_or(i64::MAX) << Q8_BITS;
    for i in 0..n {
        let qm = cont[i].map_or(q[i], |c| big_q[c]);
        let ql = match (cont[i], lat[i]) {
            (_, Some(l)) => big_q[l],
            (Some(_), None) => q[i],
            (None, None) => 0,
        };
        let denom = lambda * qm + (Q8 - lambda) * ql;
        if denom == 0 {
            continue;
        }
        let main = vigor[i] * lambda * qm / denom;
        let side = vigor[i] - main;
        match cont[i] {
            Some(c) => vigor[c] += main,
            None => bud_vigor[i] += main,
        }
        match (cont[i], lat[i]) {
            (_, Some(l)) => vigor[l] += side,
            (Some(_), None) => bud_vigor[i] += side,
            (None, None) => {}
        }
    }

    // Whole metamers per bud: the floor of its vigor, then the budget's
    // remainder to the largest fractions (ties to the lower index).
    let mut shoots: Vec<usize> = bud_vigor
        .iter()
        .map(|&v| usize::try_from(v >> Q8_BITS).unwrap_or(0).min(g.max_shoot))
        .collect();
    let left = budget.saturating_sub(shoots.iter().sum());
    let mut fractions: Vec<(i64, usize)> = (0..n)
        .filter(|&i| bud_vigor[i] > 0 && shoots[i] < g.max_shoot)
        .map(|i| (-(bud_vigor[i] & (Q8 - 1)), i))
        .collect();
    fractions.sort_unstable();
    for &(_, i) in fractions.iter().take(left) {
        shoots[i] += 1;
    }

    // 3–5. Shoots, buds in metamer order.
    let max_len = graph.len() + room;
    let mut added = 0;
    for i in 0..n {
        let shoots = shoots[i];
        if shoots == 0 {
            continue;
        }
        let terminal = cont[i].is_none();
        let first = if terminal {
            tropism(g, site, ms[i].dir, ends[i])
        } else {
            let a = ms[i].dir;
            let phase = phase(seed, axis[i]);
            let turn = (g.divergence.wrapping_mul(rank[i]).wrapping_add(phase)) % TURN;
            let p = rotate(tree::perpendicular(a), a, turn);
            let (c, s) = (
                i64::from(tree::cos(g.branch_angle)),
                i64::from(tree::sin(g.branch_angle)),
            );
            normalize(std::array::from_fn(|k| {
                i64::from(a[k]) * c + i64::from(p[k]) * s
            }))
            .unwrap_or(a)
        };
        let (mut parent, mut at, mut dir) = (i, ends[i], first);
        for k in 0..shoots {
            if out.len() >= max_len {
                break;
            }
            if k > 0 {
                dir = tropism(g, site, dir, at);
            }
            let Some(d) = clear(g, site, dir, at) else {
                let m = out.metamers()[parent];
                out.set_state(parent, BudState::Dormant, m.leaf, m.shade);
                break;
            };
            let Ok(index) = out.push(new(parent as u16, d, k == 0 && !terminal)) else {
                break;
            };
            if k == 0 && !terminal {
                let m = out.metamers()[i];
                out.set_state(i, BudState::Dead, m.leaf, m.shade);
            }
            let s = tree::step(d, g.internode_mm);
            at = [at[0] + s[0], at[1] + s[1], at[2] + s[2]];
            parent = usize::from(index);
            dir = d;
            added += 1;
        }
    }
    (out, added)
}

/// `dir` leaned toward up and up the light gradient at `at`.
fn tropism<L, P>(g: &Growth, site: &Site<L, P>, dir: Dir, at: [i32; 3]) -> Dir
where
    L: Fn(CellPos) -> i32,
    P: Fn(CellPos) -> bool,
{
    let grad = site.gradient(at).unwrap_or([0; 3]);
    let v: [i64; 3] = std::array::from_fn(|k| {
        i64::from(dir[k]) * Q8
            + i64::from(UP[k]) * g.gravitropism
            + i64::from(grad[k]) * g.phototropism
    });
    normalize(v).unwrap_or(dir)
}

/// `dir` if an internode from `at` along it ends in a passable cell, else
/// `dir` turned toward the brightest passable face of `at`'s cell if that
/// one does; `None` when both are blocked.
fn clear<L, P>(g: &Growth, site: &Site<L, P>, dir: Dir, at: [i32; 3]) -> Option<Dir>
where
    L: Fn(CellPos) -> i32,
    P: Fn(CellPos) -> bool,
{
    let ends_open = |d: Dir| {
        let s = tree::step(d, g.internode_mm);
        (site.passable)(site.cell([at[0] + s[0], at[1] + s[1], at[2] + s[2]]))
    };
    if ends_open(dir) {
        return Some(dir);
    }
    let face = site.brightest_face(at)?;
    let turned = normalize(std::array::from_fn(|k| {
        i64::from(dir[k]) + i64::from(face[k])
    }))?;
    ends_open(turned).then_some(turned)
}

/// The seeded phase of the axis starting at metamer `axis`, in [`TURN`]
/// steps.
fn phase(seed: u64, axis: usize) -> u32 {
    let mut h = Xxh3::with_seed(seed);
    h.update(b"phase");
    h.update(&(axis as u64).to_le_bytes());
    (h.digest() % u64::from(TURN)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, ChunkPos};
    use rmc_world::layers::{LayerId, PlantState};
    use rmc_world::ledger::SeasonLedger;
    use rmc_world::materials::MaterialId;
    use rmc_world::world::World;

    use crate::invariants::InvariantChecker;
    use crate::light::LightPass;
    use crate::parts::ExtendPass;
    use crate::tables::{EcoSources, EcoTableError, EcoTables, PartsDef};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    const SEED: u64 = 7;
    const OAK_FORM: &str = "form: Tree(trunk: (4, 6), crown: (2, 3)), branches: true, tuft: true,";

    /// The shipped tables with the oak grown as a graph of `lambda` and
    /// `cap`.
    fn tables(lambda: u8, cap: u16) -> EcoTables {
        tables_with(&format!(
            "form: Graph(internode_mm: 250, tip_um: 2000, branch_angle: 512, \
             divergence: 1564, lambda: {lambda}, gravitropism: 64, phototropism: 96, \
             bud_light: 32, max_shoot: 3, max_metamers: {cap}, leaf: 4),"
        ))
        .expect("the graph oak loads")
    }

    fn tables_with(form: &str) -> Result<EcoTables, EcoTableError> {
        let shipped = EcoTables::load(ASSETS).expect("Phase 2 tables load");
        let mut src = EcoSources::read(ASSETS).unwrap();
        assert!(src.plants.contains(OAK_FORM));
        src.plants = src.plants.replace(OAK_FORM, form);
        EcoTables::from_sources(Arc::clone(shipped.materials()), &src)
    }

    fn id(t: &EcoTables, key: &str) -> MaterialId {
        t.materials().id(key).unwrap()
    }

    fn oak_parts(t: &EcoTables) -> PartsDef {
        let oak = t.plants().iter().find(|p| p.key == "oak").unwrap();
        oak.parts.clone().unwrap()
    }

    /// Two chunks of loam side by side along x (the border runs between
    /// x = 31 and x = 32) up to y = 30, air from y = 31, and two of air
    /// above them.
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

    /// A ripe oak root (its wood) at `cell`.
    fn oak(w: &mut World, t: &EcoTables, cell: CellPos) {
        let species = t.plants().iter().position(|p| p.key == "oak").unwrap() as u8;
        w.eco_set(cell, id(t, "oak_wood"), &mut SeasonLedger::default());
        w.eco_put_plant(
            cell,
            Some(PlantState {
                species,
                ..PlantState::default()
            }),
        );
    }

    /// One extension sweep of world seed `seed`, checking the conservation
    /// invariants.
    fn extend(w: &mut World, t: &EcoTables, seed: u64) {
        let checker = InvariantChecker::new(w);
        let mut ledger = SeasonLedger::default();
        ExtendPass::new(t, seed).run(w, &mut ledger);
        checker.check(w, &ledger).unwrap_or_else(|v| panic!("{v}"));
    }

    /// `n` flushes with the roots' biomass topped up before each: the light
    /// pass, then the extension sweep.
    fn flushes(w: &mut World, t: &EcoTables, roots: &[CellPos], n: usize, seed: u64) {
        let light = LightPass::new(t);
        for _ in 0..n {
            for &r in roots {
                w.eco_put_layer(r, LayerId::Biomass, i32::from(u8::MAX));
            }
            light.recompute(w);
            extend(w, t, seed);
        }
    }

    fn graph(w: &World, root: CellPos) -> TreeGraph {
        w.tree(root).cloned().unwrap_or_default()
    }

    /// The cell holding a point of the tree rooted at `root`.
    fn cell_of(root: CellPos, at: [i32; 3]) -> CellPos {
        let site = Site {
            root,
            light: |_| 0,
            passable: |_| true,
        };
        site.cell(at)
    }

    /// Top and widest horizontal reach of a graph's ends, in millimeters.
    fn extent(g: &TreeGraph) -> (i32, i32) {
        let ends = g.ends();
        let top = ends.iter().map(|e| e[1]).max().unwrap_or(0);
        let wide = ends
            .iter()
            .map(|e| (i64::from(e[0]).pow(2) + i64::from(e[2]).pow(2)).isqrt() as i32)
            .max()
            .unwrap_or(0);
        (top, wide)
    }

    #[test]
    fn the_same_seed_grows_the_same_graph() {
        let t = tables(133, 4000);
        let root = CellPos::new(16, 31, 16);
        let grown = |seed| {
            let mut w = world(&t);
            oak(&mut w, &t, root);
            flushes(&mut w, &t, &[root], 10, seed);
            w
        };
        let w = grown(SEED);
        let g = graph(&w, root);
        assert!(g.len() > 30, "{} metamers", g.len());
        assert!(g.metamers().iter().any(|m| m.lateral), "it branches");
        assert_eq!(g.metamers()[0].dir, UP, "the first internode is upright");
        assert_eq!(graph(&grown(SEED), root), g);
        assert_eq!(grown(SEED).hash(), w.hash());
        assert_ne!(graph(&grown(SEED + 1), root), g, "the seed sets the phases");
    }

    #[test]
    fn a_flush_pays_for_each_metamer() {
        let t = tables(133, 4000);
        let parts = oak_parts(&t);
        let cost = i32::from(parts.cost);
        let feed = |lit: i32| lit * i32::from(parts.leaf_feed) / 256;
        let mut w = world(&t);
        let root = CellPos::new(16, 31, 16);
        oak(&mut w, &t, root);
        LightPass::new(&t).recompute(&mut w);
        // Short of the cost: nothing grows, nothing is spent.
        w.eco_put_layer(root, LayerId::Biomass, cost - 1);
        extend(&mut w, &t, SEED);
        assert!(graph(&w, root).is_empty());
        assert_eq!(w.layer(root, LayerId::Biomass), cost - 1);
        // An empty graph grows its first internode.
        w.eco_put_layer(root, LayerId::Biomass, cost + 3);
        extend(&mut w, &t, SEED);
        assert_eq!(graph(&w, root).len(), 1);
        assert_eq!(w.layer(root, LayerId::Biomass), 3);
        // Later flushes pay `cost` for every metamer they add; every leaf
        // is in open sky, so all of them feed the flush.
        for _ in 0..5 {
            let before = graph(&w, root).len();
            w.eco_put_layer(root, LayerId::Biomass, 200);
            extend(&mut w, &t, SEED);
            let added = (graph(&w, root).len() - before) as i32;
            assert!(added > 0, "{before} {:?}", graph(&w, root));
            assert_eq!(
                w.layer(root, LayerId::Biomass),
                (200 + feed(before as i32)).min(255) - added * cost
            );
        }
    }

    #[test]
    fn a_tree_across_a_chunk_border_grows_like_one_inside_a_chunk() {
        // Rule 20 for trees: the tree at x 31 reaches into both chunks, the
        // one at x 12 stays in one; both see open sky.
        let t = tables(133, 4000);
        let (inside, across) = (CellPos::new(12, 31, 16), CellPos::new(31, 31, 16));
        let mut w = world(&t);
        oak(&mut w, &t, inside);
        oak(&mut w, &t, across);
        flushes(&mut w, &t, &[inside, across], 12, SEED);
        let a = graph(&w, across);
        assert!(
            a.ends().iter().any(|&e| cell_of(across, e).0.x >= 32),
            "crosses the border"
        );
        assert!(a.len() > 30);
        assert_eq!(a, graph(&w, inside));
    }

    #[test]
    fn a_tree_under_a_roof_grows_toward_the_open_side() {
        let t = tables(133, 4000);
        let root = CellPos::new(16, 31, 16);
        let mean_x = |w: &World| {
            let ends = graph(w, root).ends();
            let n = ends.len() as i64;
            let mean = |k: usize| ends.iter().map(|e| i64::from(e[k])).sum::<i64>() / n;
            (mean(0), mean(2))
        };
        // A stone roof 2 m up over x ≤ 17; open sky from x = 18.
        let mut w = world(&t);
        let stone = id(&t, "stone");
        let mut scratch = SeasonLedger::default();
        for z in 4..=28 {
            for x in 2..=17 {
                w.eco_set(CellPos::new(x, 35, z), stone, &mut scratch);
            }
        }
        oak(&mut w, &t, root);
        flushes(&mut w, &t, &[root], 14, SEED);
        let g = graph(&w, root);
        assert!(g.len() > 20, "{} metamers", g.len());
        for &e in &g.ends() {
            assert_ne!(
                w.get_cell(cell_of(root, e)),
                Some(stone),
                "no end in the roof"
            );
        }
        let (x, z) = mean_x(&w);
        assert!(x > 2 * z.abs() && x > 250, "mean end x {x} mm, z {z} mm");
        assert!(
            g.ends().iter().any(|&e| {
                let c = cell_of(root, e).0;
                c.x >= 18 && c.y >= 35
            }),
            "it reaches past the roof's edge and up"
        );
        // In the open, the same tree stays centered.
        let mut open = world(&t);
        oak(&mut open, &t, root);
        flushes(&mut open, &t, &[root], 14, SEED);
        let (x_open, _) = mean_x(&open);
        assert!(x_open.abs() < x / 2, "open {x_open} mm, roofed {x} mm");
    }

    #[test]
    fn lambda_high_grows_a_leader_and_lambda_low_a_bush() {
        let root = CellPos::new(16, 31, 16);
        let shape = |lambda| {
            let t = tables(lambda, 4000);
            let mut w = world(&t);
            oak(&mut w, &t, root);
            flushes(&mut w, &t, &[root], 10, SEED);
            extent(&graph(&w, root))
        };
        let (leader, bush) = (shape(230), shape(40));
        assert!(leader.0 > bush.0, "taller: {leader:?} vs {bush:?}");
        // Slenderer: height over reach.
        assert!(
            i64::from(leader.0) * i64::from(bush.1) > i64::from(bush.0) * i64::from(leader.1),
            "{leader:?} vs {bush:?}"
        );
    }

    #[test]
    fn growth_stops_at_the_cap() {
        let t = tables(133, 40);
        let mut w = world(&t);
        let root = CellPos::new(16, 31, 16);
        oak(&mut w, &t, root);
        flushes(&mut w, &t, &[root], 20, SEED);
        let full = graph(&w, root);
        assert_eq!(full.len(), 40);
        // A full tree spends nothing.
        flushes(&mut w, &t, &[root], 1, SEED);
        assert_eq!(graph(&w, root), full);
        assert_eq!(w.layer(root, LayerId::Biomass), i32::from(u8::MAX));
    }

    #[test]
    fn a_graph_goes_when_its_root_does() {
        let t = tables(133, 4000);
        let mut w = world(&t);
        let root = CellPos::new(16, 31, 16);
        oak(&mut w, &t, root);
        flushes(&mut w, &t, &[root], 3, SEED);
        assert!(!graph(&w, root).is_empty());
        w.eco_set(root, MaterialId::AIR, &mut SeasonLedger::default());
        w.eco_put_plant(root, None);
        extend(&mut w, &t, SEED);
        assert!(w.tree(root).is_none());
    }

    #[test]
    fn graph_rows_are_checked() {
        let good = "form: Graph(internode_mm: 250, tip_um: 2000, branch_angle: 512, \
                    divergence: 1564, lambda: 133, gravitropism: 64, phototropism: 64, \
                    bud_light: 32, max_shoot: 3, max_metamers: 4000, leaf: 4),";
        assert!(tables_with(good).is_ok());
        for (from, to) in [
            ("internode_mm: 250", "internode_mm: 0"),
            ("branch_angle: 512", "branch_angle: 2049"),
            ("max_shoot: 3", "max_shoot: 0"),
            ("max_metamers: 4000", "max_metamers: 0"),
            ("max_metamers: 4000", "max_metamers: 65535"),
        ] {
            assert!(tables_with(&good.replace(from, to)).is_err(), "{to}");
        }
    }
}
