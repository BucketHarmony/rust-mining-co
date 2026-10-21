//! Magma chambers (SAD-P2 §8): `count_min..=count_max` blobs below
//! `chamber_max_y`, each around a core of the eco role `magma_source`,
//! never within `shell_clearance` cells of the bedrock shell.
//!
//! Chambers are planned once per world from the seed and the bounds, so a
//! cell's chamber material is a function of its world position and chunks
//! still generate independently. Every chamber cell lies inside its reach
//! (`radius + amplitude` of the center), and the planner keeps the whole
//! reach box clear of the shell, so no chamber cell is near it.

use glam::IVec3;
use rmc_eco::EcoTables;
use rmc_world::coords::{CellAabb, CellPos};
use rmc_world::materials::MaterialId;

use crate::noise::{LAYER_MAGMA, LAYER_MAGMA_BLOB, NoiseLayer, cell_hash};
use crate::params::MagmaChamberParams;

/// The eco role of the chamber core material.
const CORE_ROLE: &str = "magma_source";
/// The eco role of the chamber material.
const MAGMA_ROLE: &str = "magma";

/// What worldgen needs from the eco tables to fill magma chambers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MagmaInit {
    /// Role `magma`, if the material table has it.
    magma: Option<MaterialId>,
    /// Role `magma_source`, if the material table has it with a
    /// `heat_source`.
    core: Option<MaterialId>,
}

impl MagmaInit {
    /// From the eco tables, whose material table must be the one worldgen
    /// generates with. A core without a `heat_source` would let the chamber
    /// freeze, so it counts as missing.
    pub fn new(tables: &EcoTables) -> Self {
        let core = tables.role_material(CORE_ROLE).filter(|&id| {
            tables
                .materials()
                .get(id)
                .is_some_and(|d| d.heat_source.is_some())
        });
        Self {
            magma: tables.role_material(MAGMA_ROLE),
            core,
        }
    }

    /// `(magma, core)` when the table has both; otherwise worldgen places
    /// no chambers.
    pub fn materials(&self) -> Option<(MaterialId, MaterialId)> {
        Some((self.magma?, self.core?))
    }
}

/// One planned chamber.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chamber {
    pub center: CellPos,
    /// Before the boundary noise.
    pub radius: i32,
}

/// What a cell of a chamber holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChamberCell {
    Core,
    Magma,
}

/// The chambers of one world.
pub struct Chambers {
    list: Vec<Chamber>,
    amplitude: i32,
    core_radius: i32,
    /// Largest reach of any chamber: `radius_max + amplitude`.
    reach: i32,
    noise: NoiseLayer,
}

/// `|a - b|²` in `i64`.
fn dist2(a: IVec3, b: IVec3) -> i64 {
    let d = (a - b).as_i64vec3();
    d.dot(d)
}

/// A hash-picked value in `lo..=hi`; `lo` when the range is empty.
fn pick(seed: u64, i: i32, k: i32, lo: i32, hi: i32) -> i32 {
    if hi <= lo {
        return lo;
    }
    let span = (i64::from(hi) - i64::from(lo) + 1) as u64;
    let roll = cell_hash(seed, LAYER_MAGMA, i, k, 0) % span;
    (i64::from(lo) + roll as i64) as i32
}

impl Chambers {
    /// Plans the chambers of a world of `bounds` whose shell is
    /// `floor_cells` thick at the bottom and `wall_cells` at each x and z
    /// edge. Centers keep every cell within the largest reach at least
    /// `shell_clearance` cells from the shell on each axis and below
    /// `chamber_max_y`; a world with no room for that has no chambers.
    pub fn plan(
        seed: u64,
        p: &MagmaChamberParams,
        bounds: CellAabb,
        floor_cells: i32,
        wall_cells: i32,
    ) -> Self {
        let reach = p.radius_max + p.amplitude;
        let (lo, hi) = (bounds.min.0, bounds.max.0);
        let side = wall_cells + p.shell_clearance + reach;
        let min = IVec3::new(
            lo.x + side,
            lo.y + floor_cells + p.shell_clearance + reach,
            lo.z + side,
        );
        let max = IVec3::new(
            hi.x - side,
            (p.chamber_max_y - 1).min(hi.y) - reach,
            hi.z - side,
        );
        let list = if min.cmple(max).all() {
            let n = pick(seed, -1, 0, i32::from(p.count_min), i32::from(p.count_max));
            (0..n)
                .map(|i| Chamber {
                    center: CellPos::new(
                        pick(seed, i, 0, min.x, max.x),
                        pick(seed, i, 1, min.y, max.y),
                        pick(seed, i, 2, min.z, max.z),
                    ),
                    radius: pick(seed, i, 3, p.radius_min, p.radius_max),
                })
                .collect()
        } else {
            tracing::info!(?bounds, "no room for magma chambers");
            Vec::new()
        };
        Self {
            list,
            amplitude: p.amplitude,
            core_radius: p.core_radius,
            reach,
            noise: NoiseLayer::new(seed, LAYER_MAGMA_BLOB, &p.noise),
        }
    }

    /// The chambers, in planning order.
    pub fn list(&self) -> &[Chamber] {
        &self.list
    }

    /// Largest distance from a center to a chamber cell.
    pub fn reach(&self) -> i32 {
        self.reach
    }

    /// Whether `box_` may hold a chamber cell.
    pub fn touches(&self, box_: CellAabb) -> bool {
        let r = IVec3::splat(self.reach);
        self.list.iter().any(|c| {
            let (lo, hi) = (c.center.0 - r, c.center.0 + r);
            lo.cmple(box_.max.0).all() && hi.cmpge(box_.min.0).all()
        })
    }

    /// What world cell `c` holds: core within `core_radius` of a center,
    /// magma within `radius + offset` of one (the offset from the boundary
    /// noise), otherwise nothing. Cores win over another chamber's magma.
    pub fn cell(&self, c: IVec3) -> Option<ChamberCell> {
        let core2 = i64::from(self.core_radius).pow(2);
        let reach2 = i64::from(self.reach).pow(2);
        let mut near = false;
        for ch in &self.list {
            let d2 = dist2(c, ch.center.0);
            if d2 <= core2 {
                return Some(ChamberCell::Core);
            }
            near |= d2 <= reach2;
        }
        if !near {
            return None;
        }
        let off = self.noise.offset_3d(c.x, c.y, c.z, self.amplitude);
        self.list
            .iter()
            .any(|ch| dist2(c, ch.center.0) <= i64::from(ch.radius + off).pow(2))
            .then_some(ChamberCell::Magma)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, ChunkPos, LocalPos};
    use rmc_world::layers::LayerId;
    use rmc_world::materials::{MaterialDef, MaterialTable};

    use crate::params::WorldBounds;
    use crate::{GenParams, Generator, TemperatureInit, WaterTableInit};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    /// SAD-P2 §5's magma and magma_source rows, for tables that lack them.
    const MAGMA_ROWS: [&str; 2] = [
        r#"(id: 0, key: "magma", phase: Liquid, tags: [], hardness: 0, diggable: false,
            yield_count: 0, color: (255, 90, 0, 255), emissive: 2.0,
            viscosity: 3, conductivity: 64, heat_capacity: 32)"#,
        r#"(id: 0, key: "magma_source", phase: Solid, tags: [], hardness: 0,
            diggable: false, yield_count: 0, color: (255, 60, 0, 255), emissive: 2.0,
            heat_source: Some(13000))"#,
    ];

    fn shipped_table() -> MaterialTable {
        MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap()
    }

    /// The shipped material rows plus each of `rows` whose key the table
    /// lacks, with ids in table order.
    fn table_with(rows: &[&str]) -> Arc<MaterialTable> {
        let base = shipped_table();
        let mut defs: Vec<MaterialDef> = base.iter().cloned().collect();
        for text in rows {
            let mut d: MaterialDef = ron::from_str(text).unwrap();
            if base.id(&d.key).is_some() {
                continue;
            }
            d.id = MaterialId(defs.len() as u16);
            defs.push(d);
        }
        Arc::new(MaterialTable::from_defs(defs).unwrap())
    }

    fn tables(m: &Arc<MaterialTable>) -> EcoTables {
        EcoTables::load_with_materials(Arc::clone(m), ASSETS).unwrap()
    }

    fn shipped_params() -> GenParams {
        GenParams::load(format!("{ASSETS}/worldgen.ron")).unwrap()
    }

    fn chamber_params(p: &GenParams) -> &MagmaChamberParams {
        p.magma_chambers
            .as_ref()
            .expect("the shipped params have chambers")
    }

    /// The shipped params on a 64 × 128 × 64 world.
    fn narrow() -> GenParams {
        let mut p = shipped_params();
        p.bounds = WorldBounds {
            min: [0, 0, 0],
            max: [63, 127, 63],
        };
        p
    }

    /// `wet` and `plain` side by side, chunk for chunk over `positions`:
    /// `f(cell, plain material, wet chunk, local)`.
    fn for_each_cell(
        wet: &Generator,
        plain: &Generator,
        positions: impl IntoIterator<Item = ChunkPos>,
        mut f: impl FnMut(IVec3, MaterialId, &Chunk, LocalPos),
    ) {
        for pos in positions {
            let (w, p) = (wet.generate(pos), plain.generate(pos));
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                f(CellPos::from_chunk_local(pos, l).0, p.storage.get(l), &w, l);
            }
        }
    }

    /// The chunks of `g`'s world that may hold a chamber cell.
    fn chamber_chunks(g: &Generator) -> Vec<ChunkPos> {
        assert!(!g.chambers().is_empty(), "the world has chambers");
        let r = IVec3::splat(g.chamber_reach());
        g.bounds()
            .chunks()
            .filter(|pos| {
                let o = pos.origin_cell().0;
                g.chambers().iter().any(|c| {
                    (c.center.0 - r).cmple(o + IVec3::splat(31)).all()
                        && (c.center.0 + r).cmpge(o).all()
                })
            })
            .collect()
    }

    #[test]
    fn chambers_have_source_core_and_init_temp() {
        let m = table_with(&MAGMA_ROWS);
        let t = tables(&m);
        let init = MagmaInit::new(&t);
        let (magma, core) = init.materials().expect("the fixture has both roles");
        assert_eq!(Some(magma), m.id("magma"));
        assert_eq!(Some(core), m.id("magma_source"));
        let core_temp = m.get(core).unwrap().heat_source.unwrap();
        let magma_temp = t.params().magma_init_temp;
        let p = narrow();
        let cp = chamber_params(&p).clone();
        assert!(
            cp.core_radius < cp.radius_min - cp.amplitude,
            "a core inside magma"
        );

        // The count is 1..=3, picked by the seed, and the plan is a function
        // of the seed.
        let mut counts = BTreeSet::new();
        for seed in 0..64 {
            let g = Generator::new(seed, &p, &m).unwrap();
            let n = g.chambers().len();
            assert!((1..=3).contains(&n), "seed {seed}: {n} chambers");
            counts.insert(n);
            assert_eq!(
                g.chambers(),
                Generator::new(seed, &p, &m).unwrap().chambers()
            );
            for c in g.chambers() {
                assert!((cp.radius_min..=cp.radius_max).contains(&c.radius), "{c:?}");
            }
        }
        assert_eq!(counts, BTreeSet::from([1, 2, 3]), "every count occurs");
        let a = Generator::new(1, &p, &m).unwrap();
        let b = Generator::new(2, &p, &m).unwrap();
        assert_ne!(a.chambers(), b.chambers(), "seeds place chambers apart");

        for seed in [3, 17] {
            let wet = Generator::new(seed, &p, &m)
                .unwrap()
                .with_temperature(TemperatureInit::new(&t))
                .with_water_table(WaterTableInit::new(&t))
                .with_magma(init);
            let plain = Generator::new(seed, &p, &m)
                .unwrap()
                .with_temperature(TemperatureInit::new(&t))
                .with_water_table(WaterTableInit::new(&t));
            let chambers = wet.chambers().to_vec();
            let (mut cores, mut magmas) = (vec![0; chambers.len()], vec![0; chambers.len()]);
            let mut cells = std::collections::BTreeMap::new();
            for_each_cell(&wet, &plain, wet.bounds().chunks(), |c, was, chunk, l| {
                let got = chunk.storage.get(l);
                let temp = chunk.layers.get_i16(LayerId::Temperature, l);
                let d2: Vec<i64> = chambers.iter().map(|ch| dist2(c, ch.center.0)).collect();
                let in_core = d2.iter().any(|&d| d <= i64::from(cp.core_radius).pow(2));
                if in_core {
                    assert_eq!(got, core, "{c}: the core");
                    assert_eq!(temp, core_temp, "{c}: the core starts at its clamp");
                    for (k, &d) in d2.iter().enumerate() {
                        cores[k] += usize::from(d <= i64::from(cp.core_radius).pow(2));
                    }
                } else if got == magma {
                    assert_eq!(temp, magma_temp, "{c}: magma at magma_init_temp");
                    assert_eq!(chunk.layers.get_u8(LayerId::Moisture, l), 0, "{c}");
                    let k = d2
                        .iter()
                        .zip(&chambers)
                        .position(|(&d, ch)| d <= i64::from(ch.radius + cp.amplitude).pow(2))
                        .unwrap_or_else(|| panic!("{c}: magma outside every chamber"));
                    magmas[k] += 1;
                } else {
                    assert_ne!(got, core, "{c}: a stray core");
                    assert_eq!(got, was, "{c}: only chamber cells change");
                    for (&d, ch) in d2.iter().zip(&chambers) {
                        let inner = i64::from(ch.radius - cp.amplitude).pow(2);
                        assert!(d > inner, "{c}: a hole inside {ch:?}");
                    }
                }
                if got == magma || got == core {
                    assert!(c.y < cp.chamber_max_y, "{c}: above chamber_max_y");
                    cells.insert(c.to_array(), got);
                }
            });
            let core_cells = 1 + 6 * usize::from(cp.core_radius >= 1);
            for (k, ch) in chambers.iter().enumerate() {
                assert!(
                    cores[k] >= core_cells,
                    "seed {seed}: {ch:?} core {}",
                    cores[k]
                );
                // The inner ball, minus the core, is magma.
                let inner = (ch.radius - cp.amplitude) as f64;
                let ball = 4.0 / 3.0 * std::f64::consts::PI * inner.powi(3);
                assert!(
                    magmas[k] as f64 > ball * 0.5,
                    "seed {seed}: {ch:?} {}",
                    magmas[k]
                );
                // The core sits in magma: each face neighbour of a core cell
                // is core or magma.
                for (cell, &mat) in &cells {
                    let c = IVec3::from_array(*cell);
                    if mat != core || dist2(c, ch.center.0) > i64::from(cp.core_radius).pow(2) {
                        continue;
                    }
                    for d in [
                        IVec3::X,
                        IVec3::NEG_X,
                        IVec3::Y,
                        IVec3::NEG_Y,
                        IVec3::Z,
                        IVec3::NEG_Z,
                    ] {
                        let n = cells.get(&(c + d).to_array());
                        assert!(n.is_some(), "{c} + {d}: the core is exposed");
                    }
                }
            }

            // Without with_magma, the world is the plain one.
            let no_magma = Generator::new(seed, &p, &m)
                .unwrap()
                .with_temperature(TemperatureInit::new(&t))
                .with_water_table(WaterTableInit::new(&t));
            for pos in chamber_chunks(&wet) {
                assert_eq!(no_magma.generate(pos), plain.generate(pos), "{pos:?}");
            }
        }

        // No chamber params, a table without the roles, or a core without a
        // heat source: no chambers.
        let mut none = narrow();
        none.magma_chambers = None;
        let g = Generator::new(3, &none, &m).unwrap().with_magma(init);
        assert!(g.chambers().is_empty());
        let shipped = Arc::new(shipped_table());
        if shipped.id("magma").is_none() || shipped.id("magma_source").is_none() {
            assert_eq!(MagmaInit::new(&tables(&shipped)).materials(), None);
        }
        let mut cold: Vec<MaterialDef> = m.iter().cloned().collect();
        cold[usize::from(core.0)].heat_source = None;
        let cold = Arc::new(MaterialTable::from_defs(cold).unwrap());
        let cold_init = MagmaInit::new(&tables(&cold));
        assert_eq!(cold_init.materials(), None);
        let with = Generator::new(3, &p, &cold).unwrap().with_magma(cold_init);
        let without = Generator::new(3, &p, &cold).unwrap();
        for pos in chamber_chunks(&with) {
            assert_eq!(with.generate(pos), without.generate(pos), "{pos:?}");
        }
    }

    /// Whether world cell `c` lies in the shell of `p`.
    fn in_shell(p: &GenParams, c: IVec3) -> bool {
        let (lo, hi) = (
            IVec3::from_array(p.bounds.min),
            IVec3::from_array(p.bounds.max),
        );
        if c.cmplt(lo).any() || c.cmpgt(hi).any() {
            return false;
        }
        let w = p.bedrock.wall_cells;
        let wall = |a: usize| c[a] - lo[a] < w || hi[a] - c[a] < w;
        c.y - lo.y < p.bedrock.floor_cells || wall(0) || wall(2)
    }

    #[test]
    fn chambers_clear_of_bedrock() {
        let m = table_with(&MAGMA_ROWS);
        let t = tables(&m);
        let init = MagmaInit::new(&t);
        let (magma, core) = init.materials().unwrap();
        let shipped = shipped_params();
        let clearance = chamber_params(&shipped).shell_clearance;
        assert_eq!(clearance, 8, "SAD-P2 §8: never within 8 cells of the shell");

        // A non-aligned box with a thicker shell and a wider clearance.
        let odd = {
            let mut p = shipped
                .fit_to(WorldBounds {
                    min: [-7, -20, 5],
                    max: [70, 60, 90],
                })
                .unwrap();
            p.bedrock.floor_cells = 5;
            p.bedrock.wall_cells = 3;
            p.magma_chambers.as_mut().unwrap().shell_clearance = 10;
            p
        };
        let low = shipped
            .fit_to(WorldBounds {
                min: [0, 0, 0],
                max: [127, 63, 127],
            })
            .unwrap();
        // fit_to keeps the chambers' share of the height.
        assert_eq!(
            chamber_params(&low).chamber_max_y,
            chamber_params(&shipped).chamber_max_y / 2
        );
        let worlds = [shipped.clone(), narrow(), low.clone(), odd.clone()];

        // Every cell a chamber can reach is clear of the shell by more than
        // the clearance on each axis, and below chamber_max_y.
        for p in &worlds {
            let cp = chamber_params(p);
            let (lo, hi) = (
                IVec3::from_array(p.bounds.min),
                IVec3::from_array(p.bounds.max),
            );
            let (floor, wall) = (p.bedrock.floor_cells, p.bedrock.wall_cells);
            for seed in 0..64 {
                let g = Generator::new(seed, p, &m).unwrap();
                assert!(!g.chambers().is_empty(), "{:?} seed {seed}", p.bounds);
                for c in g.chambers() {
                    let r = c.radius + cp.amplitude;
                    let (a, b) = (c.center.0 - IVec3::splat(r), c.center.0 + IVec3::splat(r));
                    assert!(a.y - (lo.y + floor) >= cp.shell_clearance, "{c:?}: floor");
                    assert!(a.x - (lo.x + wall) >= cp.shell_clearance, "{c:?}: -x wall");
                    assert!(a.z - (lo.z + wall) >= cp.shell_clearance, "{c:?}: -z wall");
                    assert!((hi.x - wall) - b.x >= cp.shell_clearance, "{c:?}: +x wall");
                    assert!((hi.z - wall) - b.z >= cp.shell_clearance, "{c:?}: +z wall");
                    assert!(b.y < cp.chamber_max_y && b.y <= hi.y, "{c:?}: too high");
                }
            }
        }

        // Generated cells: no shell cell within the clearance of a chamber
        // cell (brute force), and the shell intact.
        for p in [&low, &odd] {
            let cp = chamber_params(p);
            let n = cp.shell_clearance;
            for seed in [5, 9] {
                let g = Generator::new(seed, p, &m).unwrap().with_magma(init);
                let plain = Generator::new(seed, p, &m).unwrap();
                let mut found = 0;
                for_each_cell(&g, &plain, chamber_chunks(&g), |c, was, chunk, l| {
                    let got = chunk.storage.get(l);
                    if in_shell(p, c) {
                        assert_eq!(got, was, "{c}: the shell is the last word");
                    }
                    if got != magma && got != core {
                        return;
                    }
                    found += 1;
                    assert!(c.y < cp.chamber_max_y, "{c}");
                    for dy in -n..=n {
                        for dz in -n..=n {
                            for dx in -n..=n {
                                let s = c + IVec3::new(dx, dy, dz);
                                assert!(!in_shell(p, s), "{c}: shell cell {s} is near");
                            }
                        }
                    }
                });
                assert!(found > 0, "{:?} seed {seed}: no chamber cells", p.bounds);
            }
        }

        // A world with no room for a chamber has none, and generates as
        // without them.
        let mut tight = narrow();
        tight.magma_chambers.as_mut().unwrap().chamber_max_y = 2 + clearance + 5;
        let g = Generator::new(4, &tight, &m).unwrap().with_magma(init);
        assert!(g.chambers().is_empty());
        let plain = Generator::new(4, &tight, &m).unwrap();
        for pos in g.bounds().chunks().filter(|c| c.0.y == 0) {
            assert_eq!(g.generate(pos), plain.generate(pos), "{pos:?}");
        }

        // Room for exactly one center: every chamber sits on it.
        let cp = chamber_params(&tight).clone();
        let reach = cp.radius_max + cp.amplitude;
        let only_y = 2 + clearance + reach;
        tight.magma_chambers.as_mut().unwrap().chamber_max_y = only_y + reach + 1;
        let side = 1 + clearance + reach;
        tight.bounds.max[0] = 2 * side;
        tight.bounds.max[2] = 2 * side;
        for seed in 0..16 {
            let g = Generator::new(seed, &tight, &m).unwrap();
            for c in g.chambers() {
                assert_eq!(c.center, CellPos::new(side, only_y, side), "seed {seed}");
            }
        }
    }
}
