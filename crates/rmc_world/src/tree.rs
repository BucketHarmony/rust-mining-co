//! Tree graphs (docs/TREES.md §4.1): a tree is a list of metamers (an
//! internode with its bud and leaf) in growth order, owned by the chunk of
//! its root cell. All math is integer: directions are Q14 unit vectors,
//! lengths and positions millimeters, radii micrometers, angles steps of
//! [`TURN`] per full turn (rule 3).
//!
//! Positions and radii are not stored. Positions are prefix sums of
//! `dir · length` from the root cell's top-face center; radii come from the
//! pipe model with exponent 2 over each subtree.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 1.0 in Q14.
pub const Q14: i32 = 1 << 14;
/// The parent of a graph's first metamer, which starts at the root.
pub const NO_PARENT: u16 = u16::MAX;
/// Angle steps per full turn.
pub const TURN: u32 = 4096;
/// The golden angle, 137.5°, in [`TURN`] steps (137.46°).
pub const GOLDEN: u32 = 1564;
/// Bytes per metamer in [`TreeGraph::encode`].
pub const METAMER_BYTES: usize = 16;
/// How far a stored direction's length may be from [`Q14`], in Q14 steps.
/// Rounding each component after normalizing moves the length by under one
/// step.
pub const UNIT_SLACK: i32 = 2;

/// A Q14 unit vector.
pub type Dir = [i16; 3];

/// Steps per quarter turn.
const QUARTER: usize = (TURN / 4) as usize;
/// Steps per eighth turn: the Taylor series covers `[0, π/4]`.
const EIGHTH: i64 = (TURN / 8) as i64;
/// Fraction bits of the series.
const SERIES_BITS: u32 = 30;
/// π in Q30.
const PI_Q30: i64 = 3_373_259_426;
/// [`normalize`] works on components of at least this many bits.
const NORMALIZE_BITS: u32 = 30;
/// Q30 to Q14.
const SERIES_TO_Q14: u32 = SERIES_BITS - 14;

/// What a metamer's bud is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BudState {
    Dormant,
    Active,
    Flower,
    Fruit,
    Dead,
}

impl BudState {
    /// The byte in [`TreeGraph::encode`].
    pub fn byte(self) -> u8 {
        match self {
            BudState::Dormant => 0,
            BudState::Active => 1,
            BudState::Flower => 2,
            BudState::Fruit => 3,
            BudState::Dead => 4,
        }
    }
}

/// One internode, its bud and its leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metamer {
    /// Index of the metamer this one grows from; [`NO_PARENT`] for the
    /// first.
    pub parent: u16,
    /// Growth direction, a Q14 unit vector.
    pub dir: Dir,
    pub length_mm: u16,
    /// The cycle it grew in.
    pub born: u16,
    pub bud: BudState,
    /// Leaf area in steps; 0 when bare.
    pub leaf: u8,
    /// Cycles in a row below the shedding light.
    pub shade: u8,
    /// Whether it starts a lateral axis (it grew from its parent's
    /// axillary bud); otherwise it continues its parent's axis.
    pub lateral: bool,
}

impl Metamer {
    fn encode_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.parent.to_le_bytes());
        for d in self.dir {
            out.extend_from_slice(&d.to_le_bytes());
        }
        out.extend_from_slice(&self.length_mm.to_le_bytes());
        out.extend_from_slice(&self.born.to_le_bytes());
        out.extend_from_slice(&[
            self.bud.byte(),
            self.leaf,
            self.shade,
            u8::from(self.lateral),
        ]);
    }
}

/// Why a metamer is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TreeError {
    #[error(
        "metamer {index} has parent {parent}; the first has none and the rest grow from earlier ones"
    )]
    BadParent { index: usize, parent: u16 },
    #[error("metamer {index} direction {dir:?} is not a Q14 unit vector")]
    NotUnit { index: usize, dir: Dir },
    #[error("a tree graph holds at most {} metamers", NO_PARENT)]
    Full,
}

/// A tree's metamers in growth order: the first has no parent and every
/// other's parent comes before it, and every direction is a unit vector.
/// Decoding checks this, so a graph from a save or a message holds it too.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<Metamer>", into = "Vec<Metamer>")]
pub struct TreeGraph {
    metamers: Vec<Metamer>,
}

impl TryFrom<Vec<Metamer>> for TreeGraph {
    type Error = TreeError;

    fn try_from(metamers: Vec<Metamer>) -> Result<Self, TreeError> {
        let mut graph = TreeGraph::default();
        for m in metamers {
            graph.push(m)?;
        }
        Ok(graph)
    }
}

impl From<TreeGraph> for Vec<Metamer> {
    fn from(graph: TreeGraph) -> Self {
        graph.metamers
    }
}

impl TreeGraph {
    /// Appends `m`, returning its index.
    pub fn push(&mut self, m: Metamer) -> Result<u16, TreeError> {
        let index = self.metamers.len();
        if index >= usize::from(NO_PARENT) {
            return Err(TreeError::Full);
        }
        let parent_ok = if index == 0 {
            m.parent == NO_PARENT
        } else {
            usize::from(m.parent) < index
        };
        if !parent_ok {
            return Err(TreeError::BadParent {
                index,
                parent: m.parent,
            });
        }
        if !is_unit(m.dir) {
            return Err(TreeError::NotUnit { index, dir: m.dir });
        }
        self.metamers.push(m);
        Ok(index as u16)
    }

    pub fn metamers(&self) -> &[Metamer] {
        &self.metamers
    }

    pub fn len(&self) -> usize {
        self.metamers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.metamers.is_empty()
    }

    /// Bytes the metamers take on the heap.
    pub fn heap_bytes(&self) -> usize {
        self.metamers.capacity() * std::mem::size_of::<Metamer>()
    }

    /// Sets the bud, leaf and shade of metamer `index`; its shape fields
    /// stay.
    pub fn set_state(&mut self, index: usize, bud: BudState, leaf: u8, shade: u8) {
        let m = &mut self.metamers[index];
        m.bud = bud;
        m.leaf = leaf;
        m.shade = shade;
    }

    /// Where each metamer ends, in millimeters from the root cell's
    /// top-face center: its start (its parent's end, or the origin) plus
    /// `dir · length`, each component rounded.
    pub fn ends(&self) -> Vec<[i32; 3]> {
        let mut ends: Vec<[i32; 3]> = Vec::with_capacity(self.metamers.len());
        for m in &self.metamers {
            let start = if m.parent == NO_PARENT {
                [0; 3]
            } else {
                ends[usize::from(m.parent)]
            };
            let d = step(m.dir, m.length_mm);
            ends.push([start[0] + d[0], start[1] + d[1], start[2] + d[2]]);
        }
        ends
    }

    /// Each metamer's radius in micrometers by the pipe model: a metamer
    /// without children has `tip_um`, and any other the square root of the
    /// sum of its children's squared radii.
    pub fn radii(&self, tip_um: u32) -> Vec<u32> {
        let tip = u64::from(tip_um) * u64::from(tip_um);
        let mut squared = vec![0u64; self.metamers.len()];
        for (i, m) in self.metamers.iter().enumerate().rev() {
            if squared[i] == 0 {
                squared[i] = tip;
            }
            if m.parent != NO_PARENT {
                squared[usize::from(m.parent)] += squared[i];
            }
        }
        squared
            .into_iter()
            .map(|s| u32::try_from(s.isqrt()).unwrap_or(u32::MAX))
            .collect()
    }

    /// Each metamer's wood radius in micrometers (docs/TREES.md T11): its
    /// [`radii`](Self::radii) by the pipe model plus `ring_um` for every
    /// cycle between its birth and the newest metamer's, so wood keeps
    /// thickening while the tree keeps growing or renewing.
    pub fn wood_radii(&self, tip_um: u32, ring_um: u32) -> Vec<u32> {
        let newest = self.metamers.iter().map(|m| m.born).max().unwrap_or(0);
        self.radii(tip_um)
            .into_iter()
            .zip(&self.metamers)
            .map(|(r, m)| {
                let age = u32::from(newest - m.born);
                r.saturating_add(ring_um.saturating_mul(age))
            })
            .collect()
    }

    /// The graph without the subtrees of the metamers `cut` marks, and
    /// which metamers went (a cut one and everything growing from it).
    /// Parents are renumbered; the order is kept.
    pub fn prune(&self, cut: &[bool]) -> (TreeGraph, Vec<bool>) {
        let n = self.metamers.len();
        let mut gone = vec![false; n];
        let mut index = vec![NO_PARENT; n];
        let mut kept = Vec::with_capacity(n);
        for (i, m) in self.metamers.iter().enumerate() {
            let parent_gone = m.parent != NO_PARENT && gone[usize::from(m.parent)];
            gone[i] = cut.get(i).copied().unwrap_or(false) || parent_gone;
            if gone[i] {
                continue;
            }
            index[i] = kept.len() as u16;
            let parent = if m.parent == NO_PARENT {
                NO_PARENT
            } else {
                index[usize::from(m.parent)]
            };
            kept.push(Metamer { parent, ..*m });
        }
        (TreeGraph { metamers: kept }, gone)
    }

    /// [`METAMER_BYTES`] little-endian bytes per metamer, in order.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.metamers.len() * METAMER_BYTES);
        for m in &self.metamers {
            m.encode_into(&mut out);
        }
        out
    }

    /// xxh3 over a prefix and [`Self::encode`].
    pub fn hash(&self) -> u64 {
        const PREFIX: &[u8] = b"rmc.tree.v1";
        let mut buf = Vec::with_capacity(PREFIX.len() + self.metamers.len() * METAMER_BYTES);
        buf.extend_from_slice(PREFIX);
        for m in &self.metamers {
            m.encode_into(&mut buf);
        }
        xxhash_rust::xxh3::xxh3_64(&buf)
    }
}

/// xxh3 over the tree map in key order: a prefix, then per tree its root's
/// local index and its graph hash.
pub fn trees_hash(trees: &BTreeMap<u16, TreeGraph>) -> u64 {
    const PREFIX: &[u8] = b"rmc.trees.v1";
    let mut buf = Vec::with_capacity(PREFIX.len() + trees.len() * 10);
    buf.extend_from_slice(PREFIX);
    for (index, graph) in trees {
        buf.extend_from_slice(&index.to_le_bytes());
        buf.extend_from_slice(&graph.hash().to_le_bytes());
    }
    xxhash_rust::xxh3::xxh3_64(&buf)
}

/// `dir · length_mm`, each component rounded: how far a metamer reaches.
pub fn step(dir: Dir, length_mm: u16) -> [i32; 3] {
    dir.map(|d| q14_round(i64::from(d) * i64::from(length_mm)) as i32)
}

/// `x / Q14`, rounded half up.
fn q14_round(x: i64) -> i64 {
    (x + i64::from(Q14 / 2)) >> 14
}

/// `a / b` for `b > 0`, rounded half away from zero.
fn div_round(a: i64, b: i64) -> i64 {
    if a >= 0 {
        (a + b / 2) / b
    } else {
        -((-a + b / 2) / b)
    }
}

/// Whether `v`'s length is within [`UNIT_SLACK`] of [`Q14`].
pub fn is_unit(v: Dir) -> bool {
    let len = length(v.map(i64::from));
    (len - i64::from(Q14)).abs() <= i64::from(UNIT_SLACK)
}

/// The integer length of `v`, rounded down.
pub fn length(v: [i64; 3]) -> i64 {
    let sq: u128 = v.iter().map(|&c| (c as i128 * c as i128) as u128).sum();
    sq.isqrt() as i64
}

/// `v` scaled to a Q14 unit vector, each component rounded; `None` for the
/// zero vector. Components are first shifted up to [`NORMALIZE_BITS`], so a
/// short `v` loses nothing to the integer length.
pub fn normalize(v: [i64; 3]) -> Option<Dir> {
    let largest = v.iter().map(|c| c.unsigned_abs()).max().unwrap_or(0);
    if largest == 0 {
        return None;
    }
    let shift = NORMALIZE_BITS.saturating_sub(largest.ilog2());
    let v = v.map(|c| c << shift);
    let len = length(v);
    Some(v.map(|c| div_round(c * i64::from(Q14), len) as i16))
}

pub fn dot(a: Dir, b: Dir) -> i64 {
    (0..3).map(|k| i64::from(a[k]) * i64::from(b[k])).sum()
}

/// `a × b`, unscaled (Q28 for Q14 inputs).
pub fn cross(a: Dir, b: Dir) -> [i64; 3] {
    let [ax, ay, az] = a.map(i64::from);
    let [bx, by, bz] = b.map(i64::from);
    [ay * bz - az * by, az * bx - ax * bz, ax * by - ay * bx]
}

/// A unit vector perpendicular to `v`: `v` crossed with the axis it leans on
/// least (the first such axis on a tie).
pub fn perpendicular(v: Dir) -> Dir {
    let mut axis = 0;
    for k in 1..3 {
        if v[k].unsigned_abs() < v[axis].unsigned_abs() {
            axis = k;
        }
    }
    let mut unit = [0i16; 3];
    unit[axis] = Q14 as i16;
    normalize(cross(v, unit)).unwrap_or(unit)
}

/// `sin(2π · steps / TURN)` in Q14.
pub fn sin(steps: u32) -> i32 {
    let s = (steps % TURN) as usize;
    let (q, r) = (s / QUARTER, s % QUARTER);
    let v = i32::from(match q {
        0 | 2 => SIN_QUARTER[r],
        _ => SIN_QUARTER[QUARTER - r],
    });
    if q < 2 { v } else { -v }
}

/// `cos(2π · steps / TURN)` in Q14.
pub fn cos(steps: u32) -> i32 {
    sin(steps.wrapping_add(TURN / 4) % TURN)
}

/// `v` rotated by `steps` about the unit `axis` (right-handed), by
/// Rodrigues' formula in integers, then renormalized.
pub fn rotate(v: Dir, axis: Dir, steps: u32) -> Dir {
    let (c, s) = (i64::from(cos(steps)), i64::from(sin(steps)));
    let q = i64::from(Q14);
    let kxv = cross(axis, v).map(q14_round);
    let kdv = q14_round(dot(axis, v));
    let along = q14_round(kdv * (q - c));
    let w: [i64; 3] =
        std::array::from_fn(|k| i64::from(v[k]) * c + kxv[k] * s + i64::from(axis[k]) * along);
    normalize(w).unwrap_or(v)
}

/// `sin` over a quarter turn, `[0, TURN / 4]` inclusive, in Q14.
static SIN_QUARTER: [i16; QUARTER + 1] = sin_quarter();

/// The quarter table: the Taylor series of sin and cos on `[0, π/4]` in
/// Q30, mirrored about π/4.
const fn sin_quarter() -> [i16; QUARTER + 1] {
    let mut table = [0i16; QUARTER + 1];
    let mut r = 0;
    while r <= QUARTER {
        let q30 = if (r as i64) <= EIGHTH {
            series(r as i64, true)
        } else {
            series(QUARTER as i64 - r as i64, false)
        };
        let half = 1i64 << (SERIES_TO_Q14 - 1);
        table[r] = ((q30 + half) >> SERIES_TO_Q14) as i16;
        r += 1;
    }
    table
}

/// `sin` (or `cos`) of `steps` (at most an eighth turn) in Q30.
const fn series(steps: i64, sine: bool) -> i64 {
    // x = 2π · steps / TURN = π · steps / (TURN / 2).
    let x = PI_Q30 * steps / (TURN as i64 / 2);
    let x2 = (x * x) >> SERIES_BITS;
    let mut term = if sine { x } else { 1i64 << SERIES_BITS };
    let mut sum = term;
    let mut n = if sine { 2 } else { 1 };
    while n < 14 {
        term = -((term * x2) >> SERIES_BITS) / (n * (n + 1));
        sum += term;
        n += 2;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP: Dir = [0, Q14 as i16, 0];

    fn m(parent: u16, dir: Dir, length_mm: u16) -> Metamer {
        Metamer {
            parent,
            dir,
            length_mm,
            born: 0,
            bud: BudState::Active,
            leaf: 0,
            shade: 0,
            lateral: false,
        }
    }

    /// A 1 m trunk with two 0.5 m limbs at 45° either side, in the x-y plane.
    pub(crate) fn y_fork() -> TreeGraph {
        let d = 11_585; // Q14 · √½
        TreeGraph::try_from(vec![
            m(NO_PARENT, UP, 1000),
            m(0, [d, d, 0], 500),
            m(0, [-d, d, 0], 500),
        ])
        .expect("a Y fork is a graph")
    }

    #[test]
    fn a_y_fork_has_exact_positions_and_pipe_radii() {
        let g = y_fork();
        assert_eq!(
            g.ends(),
            [[0, 1000, 0], [354, 1354, 0], [-354, 1354, 0]],
            "11585 · 500 / 16384 = 353.5…, rounded to 354 both ways"
        );
        // Tips 2 mm; the trunk carries both: √(2 · 2000²) = 2828.4….
        assert_eq!(g.radii(2000), [2828, 2000, 2000]);
        // A chain passes its tip radius straight down.
        let chain = TreeGraph::try_from(vec![m(NO_PARENT, UP, 100), m(0, UP, 100)]).unwrap();
        assert_eq!(chain.ends(), [[0, 100, 0], [0, 200, 0]]);
        assert_eq!(chain.radii(1500), [1500, 1500]);
    }

    #[test]
    fn wood_gains_a_ring_for_each_cycle_older_than_the_newest_metamer() {
        let born = |b: u16, mut x: Metamer| {
            x.born = b;
            x
        };
        let g = TreeGraph::try_from(vec![
            born(0, m(NO_PARENT, UP, 100)),
            born(3, m(0, UP, 100)),
            born(5, m(1, UP, 100)),
        ])
        .unwrap();
        assert_eq!(g.wood_radii(1500, 0), g.radii(1500));
        assert_eq!(g.wood_radii(1500, 100), [2000, 1700, 1500]);
        // A graph whose metamers are all one age has the pipe radii.
        assert_eq!(y_fork().wood_radii(2000, 100), y_fork().radii(2000));
    }

    #[test]
    fn graphs_refuse_bad_parents_and_directions() {
        let mut g = TreeGraph::default();
        assert_eq!(
            g.push(m(0, UP, 10)),
            Err(TreeError::BadParent {
                index: 0,
                parent: 0
            })
        );
        g.push(m(NO_PARENT, UP, 10)).unwrap();
        for parent in [1, NO_PARENT] {
            assert!(matches!(
                g.push(m(parent, UP, 10)),
                Err(TreeError::BadParent { index: 1, .. })
            ));
        }
        assert!(matches!(
            g.push(m(0, [0, 9000, 0], 10)),
            Err(TreeError::NotUnit { .. })
        ));
        assert_eq!(g.len(), 1, "refused metamers are not kept");
        // Decoding checks the same way.
        let bad = postcard::to_stdvec(&vec![m(NO_PARENT, UP, 10), m(5, UP, 10)]).unwrap();
        assert!(postcard::from_bytes::<TreeGraph>(&bad).is_err());
        let good = postcard::to_stdvec(&y_fork()).unwrap();
        assert_eq!(postcard::from_bytes::<TreeGraph>(&good).unwrap(), y_fork());
    }

    #[test]
    fn the_hash_changes_with_any_field() {
        let g = y_fork();
        assert_eq!(g.encode().len(), 3 * METAMER_BYTES);
        let base = g.hash();
        let edits: [fn(&mut Metamer); 9] = [
            |m| m.parent = 1,
            |m| m.dir = [m.dir[2], m.dir[1], m.dir[0]],
            |m| m.length_mm += 1,
            |m| m.born += 1,
            |m| m.bud = BudState::Dead,
            |m| m.leaf += 1,
            |m| m.shade += 1,
            |m| m.dir = m.dir.map(|c| -c),
            |m| m.lateral = !m.lateral,
        ];
        for (k, edit) in edits.iter().enumerate() {
            let mut metamers = g.metamers().to_vec();
            edit(&mut metamers[2]);
            let changed = TreeGraph { metamers };
            assert_ne!(changed.hash(), base, "edit {k}");
            assert_ne!(changed.encode(), g.encode(), "edit {k}");
        }
        let mut trees = BTreeMap::from([(5u16, g.clone())]);
        let one = trees_hash(&trees);
        trees.insert(6, g.clone());
        assert_ne!(trees_hash(&trees), one, "a second tree");
        let moved = BTreeMap::from([(6u16, g)]);
        assert_ne!(trees_hash(&moved), one, "the same tree at another root");
    }

    #[test]
    fn the_chunk_hash_covers_trees_only_when_there_are_some() {
        use crate::chunk::Chunk;
        use crate::materials::MaterialId;

        let mut chunk = Chunk::uniform(MaterialId::AIR);
        let bare = chunk.hash();
        assert_eq!(bare, chunk.storage.hash(), "no trees: the storage hash");
        chunk.trees.insert(7, y_fork());
        let one = chunk.hash();
        assert_ne!(one, bare);
        let mut grown = y_fork();
        grown.set_state(1, BudState::Flower, 4, 0);
        chunk.trees.insert(7, grown);
        assert_ne!(chunk.hash(), one, "a metamer's state");
        chunk.trees.clear();
        assert_eq!(chunk.hash(), bare);
    }

    #[test]
    fn pruning_takes_whole_subtrees_and_renumbers_the_rest() {
        // 0 trunk, 1 left limb, 2 right limb, 3 on the left limb, 4 trunk.
        let mut g = y_fork();
        g.push(m(1, UP, 100)).unwrap();
        g.push(m(0, UP, 100)).unwrap();
        let (pruned, gone) = g.prune(&[false, true, false, false, false]);
        assert_eq!(gone, [false, true, false, true, false]);
        let parents: Vec<u16> = pruned.metamers().iter().map(|m| m.parent).collect();
        assert_eq!(parents, [NO_PARENT, 0, 0]);
        assert_eq!(pruned.metamers()[1], g.metamers()[2]);
        assert_eq!(g.prune(&[]).0, g, "nothing cut");
        let (bare, gone) = g.prune(&[true]);
        assert!(
            bare.is_empty() && gone.iter().all(|&x| x),
            "cutting the first fells it"
        );
    }

    #[test]
    fn the_sine_table_matches_known_angles() {
        let quarter = TURN / 4;
        assert_eq!(
            [sin(0), sin(quarter), sin(2 * quarter), sin(3 * quarter)],
            [0, Q14, 0, -Q14]
        );
        assert_eq!([cos(0), cos(quarter), cos(2 * quarter)], [Q14, 0, -Q14]);
        // 16384 · sin(2π · s / 4096), rounded.
        for (s, want) in [(100, 2503), (341, 8185), (512, 11_585), (1564, 11_077)] {
            assert_eq!(sin(s), want, "sin of {s} steps");
        }
        assert_eq!(sin(TURN + 5), sin(5), "sin wraps at a full turn");
        for s in 0..TURN {
            let (a, b) = (i64::from(sin(s)), i64::from(cos(s)));
            let len2 = a * a + b * b;
            let q2 = i64::from(Q14) * i64::from(Q14);
            assert!((len2 - q2).abs() <= 2 * i64::from(Q14), "step {s}");
        }
    }

    #[test]
    fn rotations_turn_right_handed_and_stay_unit() {
        let x: Dir = [Q14 as i16, 0, 0];
        assert_eq!(rotate(x, UP, TURN / 4), [0, 0, -(Q14 as i16)]);
        assert_eq!(rotate(UP, x, TURN / 4), [0, 0, Q14 as i16]);
        assert_eq!(rotate(x, UP, 0), x);
        assert_eq!(rotate(x, UP, TURN), x);
    }

    #[test]
    fn five_golden_turns_stay_unit_within_one_step() {
        let tilted = normalize([3, 7, 2]).unwrap();
        let start = normalize([5, 1, -4]).unwrap();
        for axis in [UP, tilted, perpendicular(tilted)] {
            let mut v = perpendicular(axis);
            for turn in 1..=5 {
                v = rotate(v, axis, GOLDEN);
                let len = length(v.map(i64::from));
                assert!(
                    (len - i64::from(Q14)).abs() <= 1,
                    "axis {axis:?} turn {turn}: {v:?} has length {len}"
                );
                assert!(
                    dot(v, axis).abs() <= 3 * i64::from(Q14),
                    "stays perpendicular"
                );
            }
            let mut w = start;
            for _ in 0..5 {
                w = rotate(w, axis, GOLDEN);
                assert!((length(w.map(i64::from)) - i64::from(Q14)).abs() <= 1);
            }
            // The angle about the axis is kept: the component along it is
            // unchanged.
            let drift = (dot(w, axis) - dot(start, axis)).abs();
            assert!(drift <= 5 * i64::from(Q14));
        }
    }

    #[test]
    fn perpendicular_is_unit_and_square_to_its_input() {
        for v in [UP, [Q14 as i16, 0, 0], normalize([3, 7, 2]).unwrap()] {
            let p = perpendicular(v);
            assert!(is_unit(p), "{p:?}");
            assert!(dot(p, v).abs() <= 2 * i64::from(Q14), "{p:?} ⟂ {v:?}");
        }
    }
}
