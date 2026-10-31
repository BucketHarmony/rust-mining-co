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
//! **Flowers and fruit** (T6) are bud states on tips (metamers without a
//! continuation): [`bear`] turns lit leafy tips with an Active bud into
//! Flower or Fruit buds, which stop that tip growing; [`drop_buds`] lets
//! them go again (the bud turns Active) and says where each hung.
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
    pub shed_light: i32,
    pub shed_after: u8,
    pub leaf_age: u16,
    pub mature: usize,
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
            shed_light,
            shed_after,
            leaf_age,
            mature,
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
            shed_light: i32::from(shed_light),
            shed_after,
            leaf_age,
            mature: usize::from(mature),
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
        cell_at(self.root, at)
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

/// The cell holding the point `at` of the tree rooted at `root`, in
/// millimeters from the root cell's top-face center.
pub fn cell_at(root: CellPos, at: [i32; 3]) -> CellPos {
    let r = root.0;
    let half = CELL_SIZE_MM / 2;
    let mm = [
        r.x * CELL_SIZE_MM + half + at[0],
        (r.y + 1) * CELL_SIZE_MM + at[1],
        r.z * CELL_SIZE_MM + half + at[2],
    ];
    let [x, y, z] = mm.map(|c| c.div_euclid(CELL_SIZE_MM));
    CellPos::new(x, y, z)
}

/// How a tree shades the light pass (`eco.ron`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Canopy {
    /// Per leaf step of a metamer.
    pub leaf_attenuation: u32,
    /// Per limb of at least `solid_radius_um`.
    pub wood_attenuation: u32,
    pub solid_radius_um: u32,
}

/// Calls `shade(cell, attenuation)` for every metamer of `graph` (rooted at
/// `root`, twig radius `tip_um`) that takes light from the cell it ends
/// in: its leaves, and its wood when it is solid.
pub fn shade_cells(
    graph: &TreeGraph,
    root: CellPos,
    tip_um: u32,
    canopy: Canopy,
    mut shade: impl FnMut(CellPos, u32),
) {
    let radii = graph.radii(tip_um);
    for ((m, end), r) in graph.metamers().iter().zip(graph.ends()).zip(radii) {
        let solid = canopy.solid_radius_um > 0 && r >= canopy.solid_radius_um;
        let a = u32::from(m.leaf) * canopy.leaf_attenuation
            + if solid { canopy.wood_attenuation } else { 0 };
        if a > 0 {
            shade(cell_at(root, end), a);
        }
    }
}

/// The tree's state for a flush before it feeds and grows: every metamer
/// carries `leaf` while it is younger than `leaf_age` seasons (any age for
/// 0), and none in a `bare` season; its `shade` counts the flushes in a
/// row its end has had less than `shed_light`; and a lateral branch whose
/// every metamer has counted `shed_after` (when not 0) is shed. Returns the
/// new graph and a point for every leaf that fell, then for every metamer
/// shed and every leaf on it, where each leaves litter.
pub fn tend<L, P>(
    graph: &TreeGraph,
    site: &Site<L, P>,
    g: &Growth,
    bare: bool,
    season: u16,
) -> (TreeGraph, Vec<[i32; 3]>)
where
    L: Fn(CellPos) -> i32,
    P: Fn(CellPos) -> bool,
{
    let ends = graph.ends();
    let mut ms = graph.metamers().to_vec();
    let mut fallen = Vec::new();
    for (m, &end) in ms.iter_mut().zip(&ends) {
        let young = g.leaf_age == 0 || season.saturating_sub(m.born) < g.leaf_age;
        let leaf = if bare || !young { 0 } else { g.leaf };
        if m.leaf > 0 && leaf == 0 {
            fallen.push(end);
        }
        m.leaf = leaf;
        m.shade = if site.light_at(end) < g.shed_light {
            m.shade.saturating_add(1)
        } else {
            0
        };
    }
    let tended = TreeGraph::try_from(ms).expect("the shape is unchanged");
    if g.shed_after == 0 {
        return (tended, fallen);
    }
    // The least shade count over each subtree.
    let ms = tended.metamers();
    let mut least: Vec<u8> = ms.iter().map(|m| m.shade).collect();
    for i in (1..ms.len()).rev() {
        let p = usize::from(ms[i].parent);
        least[p] = least[p].min(least[i]);
    }
    let cut: Vec<bool> = ms
        .iter()
        .zip(&least)
        .map(|(m, &l)| m.lateral && l >= g.shed_after)
        .collect();
    let (pruned, gone) = tended.prune(&cut);
    for (i, m) in ms.iter().enumerate() {
        if gone[i] {
            fallen.push(ends[i]);
            if m.leaf > 0 {
                fallen.push(ends[i]);
            }
        }
    }
    (pruned, fallen)
}

/// How many of the graph's buds are in state `bud`.
pub fn count(graph: &TreeGraph, bud: BudState) -> usize {
    graph.metamers().iter().filter(|m| m.bud == bud).count()
}

/// Turns up to `want` tips of `graph` into `bud` buds (Flower or Fruit):
/// tips whose bud is Active and that carry leaves, ending in a cell with at
/// least `light`, the least seeded roll of `(seed, bud, index)` first.
/// Returns the new graph and how many it turned.
pub fn bear<L, P>(
    graph: &TreeGraph,
    site: &Site<L, P>,
    bud: BudState,
    want: usize,
    light: i32,
    seed: u64,
) -> (TreeGraph, usize)
where
    L: Fn(CellPos) -> i32,
    P: Fn(CellPos) -> bool,
{
    let ms = graph.metamers();
    let mut tip = vec![true; ms.len()];
    for m in ms.iter().filter(|m| !m.lateral && m.parent != NO_PARENT) {
        tip[usize::from(m.parent)] = false;
    }
    let ends = graph.ends();
    let mut open: Vec<(u64, usize)> = (0..ms.len())
        .filter(|&i| {
            tip[i]
                && ms[i].bud == BudState::Active
                && ms[i].leaf > 0
                && site.light_at(ends[i]) >= light
        })
        .map(|i| {
            let mut h = Xxh3::with_seed(seed);
            h.update(&[bud.byte()]);
            h.update(&(i as u64).to_le_bytes());
            (h.digest(), i)
        })
        .collect();
    open.sort_unstable();
    let mut out = graph.clone();
    let set = want.min(open.len());
    for &(_, i) in &open[..set] {
        let m = ms[i];
        out.set_state(i, bud, m.leaf, m.shade);
    }
    (out, set)
}

/// Lets go of every bud `drops` picks: its metamer's bud turns Active
/// again. Returns the new graph and, in metamer order, each dropped bud's
/// state and the point it hung at (its metamer's end).
pub fn drop_buds(
    graph: &TreeGraph,
    drops: impl Fn(BudState) -> bool,
) -> (TreeGraph, Vec<(BudState, [i32; 3])>) {
    let mut out = graph.clone();
    let mut dropped = Vec::new();
    let ends = graph.ends();
    for (i, m) in graph.metamers().iter().enumerate() {
        if drops(m.bud) {
            dropped.push((m.bud, ends[i]));
            out.set_state(i, BudState::Active, m.leaf, m.shade);
        }
    }
    (out, dropped)
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
    use crate::tables::fixtures::set_form;
    use crate::tables::{EcoSources, EcoTableError, EcoTables, PartsDef};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    const SEED: u64 = 7;

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
        set_form(&mut src, "oak", form);
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

    /// The oak as a graph that sheds: `shed_light` 255, so any cell short
    /// of open sky is shade, `shed_after` flushes; `leaf` per metamer, kept
    /// `leaf_age` seasons.
    fn shedding_tables(shed_after: u8, leaf: u8, leaf_age: u16) -> EcoTables {
        tables_with(&format!(
            "form: Graph(internode_mm: 250, tip_um: 2000, branch_angle: 512, \
             divergence: 1564, lambda: 133, gravitropism: 64, phototropism: 96, \
             bud_light: 32, max_shoot: 3, max_metamers: 4000, leaf: {leaf}, \
             shed_light: 255, shed_after: {shed_after}, leaf_age: {leaf_age}),"
        ))
        .expect("the shedding oak loads")
    }

    fn metamer(parent: u16, dir: Dir, born: u16, lateral: bool, leaf: u8) -> Metamer {
        Metamer {
            parent,
            dir,
            length_mm: 250,
            born,
            bud: BudState::Dormant,
            leaf,
            shade: 0,
            lateral,
        }
    }

    /// One extension sweep in season `season`, checking the conservation
    /// invariants (litter is ledgered as `litter_in`).
    fn extend_in(w: &mut World, t: &EcoTables, season: u32) {
        let checker = InvariantChecker::new(w);
        let mut ledger = SeasonLedger::default();
        ExtendPass::for_season(t, season, SEED).run(w, &mut ledger);
        checker.check(w, &ledger).unwrap_or_else(|v| panic!("{v}"));
    }

    fn organic(t: &EcoTables) -> LayerId {
        LayerId::Substance(t.role_substance("organic").expect("an organic role"))
    }

    fn metamer_litter(t: &EcoTables) -> i32 {
        i32::from(t.params().metamer_litter.expect("eco.metamer_litter"))
    }

    #[test]
    fn a_crown_darkens_the_ground_under_it() {
        let t = tables(133, 4000);
        let mut w = world(&t);
        let root = CellPos::new(16, 31, 16);
        oak(&mut w, &t, root);
        flushes(&mut w, &t, &[root], 14, SEED);
        assert!(w.light_dirty(), "a growing tree dirties the light");
        let light = LightPass::new(&t);
        light.recompute(&mut w);
        let ground: Vec<CellPos> = (14..=18)
            .flat_map(|z| (14..=18).map(move |x| CellPos::new(x, 31, z)))
            .filter(|&c| c != root)
            .collect();
        let shaded: Vec<i32> = ground.iter().map(|&c| w.layer(c, LayerId::Light)).collect();
        // The same ground without the tree.
        let tree = w.tree(root).cloned();
        w.eco_put_tree(root, None);
        light.recompute(&mut w);
        let open: Vec<i32> = ground.iter().map(|&c| w.layer(c, LayerId::Light)).collect();
        assert!(shaded.iter().zip(&open).all(|(s, o)| s <= o));
        assert!(
            shaded.iter().zip(&open).any(|(s, o)| s < o),
            "shaded {shaded:?}, open {open:?}"
        );
        // Leaves in their cells darken them too.
        w.eco_put_tree(root, tree.clone());
        light.recompute(&mut w);
        let g = tree.unwrap();
        let top = g
            .ends()
            .into_iter()
            .map(|e| cell_of(root, e))
            .min_by_key(|c| w.layer(*c, LayerId::Light))
            .unwrap();
        assert!(w.layer(top, LayerId::Light) < i32::from(u8::MAX));
    }

    #[test]
    fn a_shaded_lower_limb_is_shed_and_its_mass_becomes_litter() {
        let shed_after = 3;
        let t = shedding_tables(shed_after, 0, 0);
        let mut w = world(&t);
        let root = CellPos::new(16, 31, 16);
        oak(&mut w, &t, root);
        // A trunk of six, limb A (+x) from the second metamer, limb B (−x)
        // from the third, three metamers each.
        let (east, west) = ([Q14 as i16, 0, 0], [-(Q14 as i16), 0, 0]);
        let mut g = TreeGraph::default();
        g.push(metamer(NO_PARENT, UP, 0, false, 0)).unwrap();
        for p in 0..5 {
            g.push(metamer(p, UP, 0, false, 0)).unwrap();
        }
        let a = g.push(metamer(1, east, 0, true, 0)).unwrap();
        g.push(metamer(a, east, 0, false, 0)).unwrap();
        g.push(metamer(a + 1, east, 0, false, 0)).unwrap();
        let b = g.push(metamer(2, west, 0, true, 0)).unwrap();
        g.push(metamer(b, west, 0, false, 0)).unwrap();
        g.push(metamer(b + 1, west, 0, false, 0)).unwrap();
        // A stone slab over limb A only.
        let a_cells: Vec<CellPos> = g.ends()[6..9].iter().map(|&e| cell_of(root, e)).collect();
        let slab_y = a_cells.iter().map(|c| c.0.y).max().unwrap() + 1;
        let stone = id(&t, "stone");
        let mut scratch = SeasonLedger::default();
        for z in 14..=18 {
            for x in 17..=20 {
                w.eco_set(CellPos::new(x, slab_y, z), stone, &mut scratch);
            }
        }
        w.eco_put_tree(root, Some(g.clone()));
        let litter_under_a = |w: &World| -> i32 {
            let mut columns: Vec<CellPos> = a_cells
                .iter()
                .map(|c| CellPos::new(c.0.x, 30, c.0.z))
                .collect();
            columns.dedup();
            columns.iter().map(|&c| w.layer(c, organic(&t))).sum()
        };
        let before = litter_under_a(&w);
        let light = LightPass::new(&t);
        for flush in 1..=shed_after {
            light.recompute(&mut w);
            extend(&mut w, &t, SEED);
            let now = graph(&w, root);
            if flush < shed_after {
                assert_eq!(now.len(), 12, "flush {flush}");
                assert_eq!(now.metamers()[a as usize].shade, flush);
                assert_eq!(now.metamers()[b as usize].shade, 0, "limb B is in the open");
            }
        }
        let now = graph(&w, root);
        assert_eq!(now.len(), 9, "limb A is shed");
        let kept: Vec<Dir> = now.metamers().iter().map(|m| m.dir).collect();
        assert!(!kept.contains(&east) && kept.iter().filter(|&&d| d == west).count() == 3);
        assert_eq!(litter_under_a(&w), before + 3 * metamer_litter(&t));
    }

    #[test]
    fn leaves_fall_in_the_shed_season_and_grow_back_on_young_wood() {
        let t = shedding_tables(0, 4, 8);
        let rows = &t.seasons().seasons;
        let winter = rows.iter().position(|s| s.key == "winter").unwrap() as u32;
        let len = rows.len() as u32;
        // A winter three years in, and the season after it.
        let winter = 3 * len + winter;
        let spring = winter + 1;
        let young = (winter - 1) as u16;
        let mut w = world(&t);
        let root = CellPos::new(16, 31, 16);
        oak(&mut w, &t, root);
        let mut g = TreeGraph::default();
        g.push(metamer(NO_PARENT, UP, 0, false, 4)).unwrap();
        g.push(metamer(0, UP, 0, false, 4)).unwrap();
        g.push(metamer(1, UP, young, false, 4)).unwrap();
        g.push(metamer(2, UP, young, false, 4)).unwrap();
        w.eco_put_tree(root, Some(g));
        let soil = CellPos::new(16, 30, 16);
        let before = w.layer(soil, organic(&t));
        LightPass::new(&t).recompute(&mut w);

        extend_in(&mut w, &t, winter);
        let leaves: Vec<u8> = graph(&w, root).metamers().iter().map(|m| m.leaf).collect();
        assert_eq!(leaves, [0, 0, 0, 0], "bare in winter");
        assert_eq!(
            w.layer(soil, organic(&t)),
            before + 4 * metamer_litter(&t),
            "four leaves rot under the trunk"
        );

        extend_in(&mut w, &t, spring);
        let leaves: Vec<u8> = graph(&w, root).metamers().iter().map(|m| m.leaf).collect();
        assert_eq!(leaves, [0, 0, 4, 4], "only young wood leafs out");
        assert_eq!(w.layer(soil, organic(&t)), before + 4 * metamer_litter(&t));
    }

    /// The graph oak bearing from `mature` metamers, its fruit taking root
    /// with chance `take`, and with `flower` up to three flowers in spring.
    fn bearing_tables(mature: u16, take: u8, flower: bool) -> EcoTables {
        let shipped = EcoTables::load(ASSETS).expect("Phase 2 tables load");
        let mut src = EcoSources::read(ASSETS).unwrap();
        let form = format!(
            "form: Graph(internode_mm: 250, tip_um: 2000, branch_angle: 512, \
             divergence: 1564, lambda: 133, gravitropism: 64, phototropism: 96, \
             bud_light: 32, max_shoot: 3, max_metamers: 4000, leaf: 4, \
             mature: {mature}),"
        );
        let fruit = "fruit: Some((grow: \"autumn\", fall: \"winter\", count: 6, take: 48))";
        assert!(src.plants.contains(fruit));
        set_form(&mut src, "oak", &form);
        let flowers = if flower {
            ", flower: Some((material: \"clematis_flower\", seasons: [\"spring\"], count: 3))"
        } else {
            ""
        };
        src.plants = src.plants.replace(
            fruit,
            &format!(
                "fruit: Some((grow: \"autumn\", fall: \"winter\", count: 6, take: {take})){flowers}"
            ),
        );
        EcoTables::from_sources(Arc::clone(shipped.materials()), &src).expect("the bearing oak")
    }

    /// The absolute season of row `key` in year `year`.
    fn season_of(t: &EcoTables, key: &str, year: u32) -> u32 {
        let rows = &t.seasons().seasons;
        let row = rows.iter().position(|s| s.key == key).unwrap() as u32;
        year * rows.len() as u32 + row
    }

    /// Wet, nitrogen-fed soil under the whole world, so seeds can start.
    fn feed_soil(w: &mut World, t: &EcoTables) {
        let n = LayerId::Substance(t.substance_index("n").unwrap());
        for z in 0..32 {
            for x in 0..64 {
                let anchor = CellPos::new(x, 30, z);
                w.eco_put_layer(anchor, LayerId::Moisture, 100);
                w.eco_put_layer(anchor, n, 32);
            }
        }
    }

    /// One flush in season `season` with the root's biomass topped up: the
    /// light, then the extension sweep (checked). Returns its ledger.
    fn flush_in(w: &mut World, t: &EcoTables, root: CellPos, season: u32) -> SeasonLedger {
        w.eco_put_layer(root, LayerId::Biomass, i32::from(u8::MAX));
        LightPass::new(t).recompute(w);
        let checker = InvariantChecker::new(w);
        let mut ledger = SeasonLedger::default();
        ExtendPass::for_season(t, season, SEED).run(w, &mut ledger);
        checker.check(w, &ledger).unwrap_or_else(|v| panic!("{v}"));
        ledger
    }

    /// The metamers of the tree at `root` whose bud is `bud`.
    fn buds(w: &World, root: CellPos, bud: BudState) -> Vec<usize> {
        let g = graph(w, root);
        (0..g.len())
            .filter(|&i| g.metamers()[i].bud == bud)
            .collect()
    }

    #[test]
    fn a_mature_crown_fruits_in_its_season_and_fallen_fruit_takes_root_or_rots() {
        for (take, rooted) in [(0, false), (255, true)] {
            let t = bearing_tables(0, take, false);
            let mut w = world(&t);
            feed_soil(&mut w, &t);
            let root = CellPos::new(16, 31, 16);
            oak(&mut w, &t, root);
            for year in 0..2 {
                flush_in(&mut w, &t, root, season_of(&t, "spring", year));
                flush_in(&mut w, &t, root, season_of(&t, "summer", year));
            }
            assert!(
                buds(&w, root, BudState::Fruit).is_empty(),
                "none out of season"
            );
            assert!(graph(&w, root).len() > 30);

            // Autumn: six fruit at once, on leafy tips, and those tips stop.
            let autumn = season_of(&t, "autumn", 1);
            flush_in(&mut w, &t, root, autumn);
            let fruit = buds(&w, root, BudState::Fruit);
            assert_eq!(fruit.len(), 6);
            let g = graph(&w, root);
            let tip = |i: usize| {
                !g.metamers()
                    .iter()
                    .any(|m| usize::from(m.parent) == i && !m.lateral)
            };
            for &i in &fruit {
                assert!(tip(i) && g.metamers()[i].leaf > 0, "fruit on leafy tip {i}");
            }
            flush_in(&mut w, &t, root, autumn);
            assert_eq!(buds(&w, root, BudState::Fruit), fruit, "the count holds");
            let g = graph(&w, root);
            for &i in &fruit {
                assert!(
                    !g.metamers()
                        .iter()
                        .any(|m| usize::from(m.parent) == i && !m.lateral),
                    "a fruiting tip does not grow on"
                );
            }

            // Winter: the fruit falls from where it hung. With take 0 it
            // rots; with 255 acorns start below the crown.
            let ledger = flush_in(&mut w, &t, root, season_of(&t, "winter", 1));
            assert!(buds(&w, root, BudState::Fruit).is_empty());
            for i in fruit {
                assert_eq!(graph(&w, root).metamers()[i].bud, BudState::Active);
            }
            let born: Vec<CellPos> = (0..32)
                .flat_map(|z| (0..64).map(move |x| CellPos::new(x, 31, z)))
                .filter(|&c| c != root && w.plant(c).is_some())
                .collect();
            assert_eq!(born.len() as u32, ledger.plants_born);
            assert_eq!(!born.is_empty(), rooted, "take {take}: {born:?}");
            for &c in &born {
                assert_eq!(w.get_cell(c), Some(id(&t, "acorn")));
            }
        }
    }

    #[test]
    fn fallen_fruit_rots_as_part_litter() {
        let t = bearing_tables(0, 0, false);
        let part_litter = i32::from(t.params().part_litter.unwrap());
        let mut w = world(&t);
        let root = CellPos::new(16, 31, 16);
        oak(&mut w, &t, root);
        for year in 0..2 {
            flush_in(&mut w, &t, root, season_of(&t, "summer", year));
        }
        flush_in(&mut w, &t, root, season_of(&t, "autumn", 1));
        let g = graph(&w, root);
        let fruit = buds(&w, root, BudState::Fruit);
        let leafy = g.metamers().iter().filter(|m| m.leaf > 0).count() as i64;
        // Winter drops the fruit (rotting) and every leaf (bare).
        let ledger = flush_in(&mut w, &t, root, season_of(&t, "winter", 1));
        let litter = ledger.litter_in[usize::from(t.role_substance("organic").unwrap())];
        assert_eq!(
            litter,
            fruit.len() as i64 * i64::from(part_litter) + leafy * i64::from(metamer_litter(&t))
        );
        assert_eq!(ledger.plants_born, 0);
    }

    #[test]
    fn flowers_open_on_lit_tips_in_their_season_and_wilt_after() {
        let t = bearing_tables(20, 48, true);
        let part_litter = i64::from(t.params().part_litter.unwrap());
        let mut w = world(&t);
        let root = CellPos::new(16, 31, 16);
        oak(&mut w, &t, root);
        for _ in 0..6 {
            flush_in(&mut w, &t, root, season_of(&t, "spring", 0));
        }
        let flowers = buds(&w, root, BudState::Flower);
        assert_eq!(flowers.len(), 3);
        let g = graph(&w, root);
        let ends = g.ends();
        let leaf_light = i32::from(oak_parts(&t).leaf_light);
        for &i in &flowers {
            assert!(w.layer(cell_of(root, ends[i]), LayerId::Light) >= leaf_light);
        }
        let ledger = flush_in(&mut w, &t, root, season_of(&t, "summer", 0));
        assert!(buds(&w, root, BudState::Flower).is_empty(), "wilted");
        let organic = usize::from(t.role_substance("organic").unwrap());
        assert_eq!(ledger.litter_in[organic], 3 * part_litter);
    }

    #[test]
    fn a_young_tree_bears_nothing() {
        let t = bearing_tables(4000, 48, true);
        let mut w = world(&t);
        let root = CellPos::new(16, 31, 16);
        oak(&mut w, &t, root);
        for year in 0..2 {
            for key in ["spring", "summer", "autumn"] {
                flush_in(&mut w, &t, root, season_of(&t, key, year));
                assert!(buds(&w, root, BudState::Flower).is_empty());
                assert!(buds(&w, root, BudState::Fruit).is_empty());
            }
        }
        assert!(graph(&w, root).len() > 30);
    }

    #[test]
    fn an_old_cell_tree_turns_into_a_growing_graph() {
        // A save from before the oak grew graphs: a cell oak of trunk and
        // crown parts.
        let shipped = EcoTables::load(ASSETS).unwrap();
        let cells = EcoTables::from_sources(
            Arc::clone(shipped.materials()),
            &crate::tables::fixtures::cell_tree_sources(ASSETS),
        )
        .unwrap();
        let mut w = world(&cells);
        let root = CellPos::new(16, 31, 16);
        oak(&mut w, &cells, root);
        for _ in 0..12 {
            w.eco_put_layer(root, LayerId::Biomass, i32::from(u8::MAX));
            extend(&mut w, &cells, SEED);
        }
        let old = crate::parts::parts_of(&w, root).len();
        assert!(old >= 8, "{old} parts");
        assert!(graph(&w, root).is_empty());

        // The graph oak's first sweep drops every part as litter, then the
        // root grows its graph.
        let t = tables(133, 4000);
        let part_litter = i64::from(t.params().part_litter.unwrap());
        let ledger = flush_in(&mut w, &t, root, season_of(&t, "spring", 1));
        assert!(crate::parts::parts_of(&w, root).is_empty());
        crate::parts::check_parts(&w).unwrap();
        let organic = usize::from(t.role_substance("organic").unwrap());
        assert_eq!(ledger.litter_in[organic], old as i64 * part_litter);
        assert_eq!(w.get_cell(root), Some(id(&t, "oak_wood")), "the root stays");
        assert!(!graph(&w, root).is_empty(), "and grows a graph");
        flush_in(&mut w, &t, root, season_of(&t, "summer", 1));
        assert!(graph(&w, root).len() > 1);
    }
}
