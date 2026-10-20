//! Temperature (SAD-P2 §7.1), a Jacobi pass.
//!
//! Before diffusion, three clamps fix a cell's temperature: air, water and
//! ice at or above their column's `surface_y` take the season's `air_temp`,
//! `heat_source` materials their value, and the bottom cell layer of the
//! world `profile(bottom_y)`. A clamped cell keeps its clamp value; it is a
//! reservoir for its neighbors.
//!
//! Every face pair `(i, j)` carries the flux `f = (k · (T_j − T_i)) /
//! 2^heat_shift` with `k = min(cond_i, cond_j)`, in `i32` with truncating
//! division, so `f_ij = −f_ji` exactly. The edge moves `q = (f / m) · m`
//! units of energy with `m = max(C_i, C_j)`: `T_i += q / C_i`, `T_j −= q /
//! C_j`. Capacities are powers of two, so both divisions are exact and
//! `Σ T·C` is conserved by every edge. Both chunks of a face compute the
//! edge from the same pair, so the pass stays Jacobi (rule 20).
//!
//! The freeze-thaw bit (§7.5) is not set here yet; F110 adds it.

use rmc_world::coords::{CHUNK_SIZE, CellPos, LocalPos};
use rmc_world::layers::LayerId;
use rmc_world::materials::{MaterialId, Phase};

use crate::runner::{ChunkView, JacobiOut, JacobiPass};
use crate::tables::{EcoParams, EcoTables};

/// Edge length of the computed box: a chunk plus its 1-cell border.
const SPAN: i32 = CHUNK_SIZE + 2;
const SPAN_CELLS: usize = (SPAN * SPAN * SPAN) as usize;

/// The six face neighbors.
const FACES: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
    [-1, 0, 0],
    [1, 0, 0],
];

/// The geothermal profile (SAD-P2 §8): `profile(y) = geothermal_base +
/// (bottom_y − y) · gradient`, in 0.1 °C, saturated to `i16`. Worldgen
/// writes it below the surface; the temperature pass clamps the bottom
/// cell layer to `profile(bottom_y)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub geothermal_base: i16,
    pub gradient: i16,
}

impl Profile {
    /// `eco.geothermal_base` and `eco.gradient`.
    pub fn new(params: &EcoParams) -> Self {
        Self {
            geothermal_base: params.geothermal_base,
            gradient: params.gradient,
        }
    }

    /// `profile(y)` for a world whose bottom cell layer is `bottom_y`.
    pub fn at(&self, bottom_y: i32, y: i32) -> i16 {
        let t = i64::from(self.geothermal_base)
            + (i64::from(bottom_y) - i64::from(y)) * i64::from(self.gradient);
        saturate(t)
    }
}

fn saturate(t: i64) -> i16 {
    t.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

/// One cell as the pass sees it: temperature after the clamps, conductivity
/// and heat capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeatCell {
    pub t: i32,
    pub k: i32,
    pub c: i32,
}

/// The flux from `j` into `i`: `(min(k_i, k_j) · (T_j − T_i)) / 2^heat_shift`,
/// truncating, so `flux(i, j, s) == -flux(j, i, s)`.
pub fn flux(i: HeatCell, j: HeatCell, heat_shift: u8) -> i32 {
    let k = i.k.min(j.k);
    (k * (j.t - i.t)) / (1 << heat_shift)
}

/// The change of `T_i` across the edge to `j`: `q / C_i` with `q = (f /
/// m) · m`, `m = max(C_i, C_j)`. `C_i · edge_delta(i, j) == −C_j ·
/// edge_delta(j, i)` for power-of-two capacities: the edge conserves
/// energy.
pub fn edge_delta(i: HeatCell, j: HeatCell, heat_shift: u8) -> i32 {
    let m = i.c.max(j.c);
    let q = (flux(i, j, heat_shift) / m) * m;
    q / i.c
}

/// The temperature pass for one season.
#[derive(Clone, Debug)]
pub struct TemperaturePass {
    /// `eco.heat_shift`.
    pub heat_shift: u8,
    /// The season's `air_temp`: the clamp for surface and sky-open air,
    /// water and ice.
    pub air_temp: i16,
    /// The bottom clamp is `profile.at(bottom_y, bottom_y)`.
    pub profile: Profile,
    /// Role `water`.
    pub water: Option<MaterialId>,
    /// Role `ice`.
    pub ice: Option<MaterialId>,
}

impl TemperaturePass {
    /// The pass for season `season` (row `season % len` of `seasons.ron`).
    pub fn new(tables: &EcoTables, season: u32) -> Self {
        let seasons = &tables.seasons().seasons;
        let row = &seasons[season as usize % seasons.len()];
        Self {
            heat_shift: tables.params().heat_shift,
            air_temp: row.air_temp,
            profile: Profile::new(tables.params()),
            water: tables.role_material("water"),
            ice: tables.role_material("ice"),
        }
    }

    /// The clamp of a cell, if any, first match in SAD-P2 §7.1 order.
    fn clamp(
        &self,
        view: &ChunkView<'_>,
        cell: CellPos,
        m: MaterialId,
        surface_y: i16,
    ) -> Option<i16> {
        let def = view.materials().get(m)?;
        let y = cell.0.y;
        let fluid = def.phase == Phase::Air || Some(m) == self.water || Some(m) == self.ice;
        if fluid && y >= i32::from(surface_y) {
            return Some(self.air_temp);
        }
        if let Some(t) = def.heat_source {
            return Some(t);
        }
        let bottom_y = view.bounds().min.0.y;
        (y == bottom_y).then(|| self.profile.at(bottom_y, bottom_y))
    }
}

/// Index into the 34³ box, coordinates relative to the chunk origin in
/// `-1..=32`.
fn span_index(x: i32, y: i32, z: i32) -> usize {
    ((x + 1) + (z + 1) * SPAN + (y + 1) * SPAN * SPAN) as usize
}

impl JacobiPass for TemperaturePass {
    fn compute(&self, view: &ChunkView<'_>) -> JacobiOut {
        let origin = view.pos().origin_cell().0;
        let mut surface = vec![0i16; (SPAN * SPAN) as usize];
        for z in -1..=CHUNK_SIZE {
            for x in -1..=CHUNK_SIZE {
                surface[((x + 1) + (z + 1) * SPAN) as usize] =
                    view.surface_y(origin.x + x, origin.z + z);
            }
        }
        // Every cell of the box after the clamps; `None` where there is no
        // cell (out of bounds or not resident). `fixed` marks clamped cells.
        let mut cells: Vec<Option<HeatCell>> = vec![None; SPAN_CELLS];
        let mut fixed = vec![false; SPAN_CELLS];
        // The box clipped to the bounds; cells outside stay `None`.
        let b = view.bounds();
        let span = |o: i32, min: i32, max: i32| (min - o).max(-1)..=(max - o).min(CHUNK_SIZE);
        let ys = span(origin.y, b.min.0.y, b.max.0.y);
        let zs = span(origin.z, b.min.0.z, b.max.0.z);
        let xs = span(origin.x, b.min.0.x, b.max.0.x);
        for y in ys {
            for z in zs.clone() {
                for x in xs.clone() {
                    let cell = CellPos::new(origin.x + x, origin.y + y, origin.z + z);
                    let Some(m) = view.material(cell) else {
                        continue;
                    };
                    let Some(def) = view.materials().get(m) else {
                        continue;
                    };
                    let i = span_index(x, y, z);
                    let sy = surface[((x + 1) + (z + 1) * SPAN) as usize];
                    let clamp = self.clamp(view, cell, m, sy);
                    fixed[i] = clamp.is_some();
                    let t = clamp.map_or_else(|| view.layer(cell, LayerId::Temperature), i32::from);
                    cells[i] = Some(HeatCell {
                        t,
                        k: i32::from(def.conductivity),
                        c: i32::from(def.heat_capacity),
                    });
                }
            }
        }
        let mut out = JacobiOut::default();
        for (idx, cell) in view.cells() {
            let l = LocalPos::from_index(idx).0.as_ivec3();
            let i = span_index(l.x, l.y, l.z);
            let Some(me) = cells[i] else {
                continue;
            };
            let new = if fixed[i] {
                me.t
            } else {
                let mut sum = 0;
                for d in FACES {
                    if let Some(other) = cells[span_index(l.x + d[0], l.y + d[1], l.z + d[2])] {
                        sum += edge_delta(me, other, self.heat_shift);
                    }
                }
                i32::from(saturate(i64::from(me.t) + i64::from(sum)))
            };
            let delta = new - view.layer(cell, LayerId::Temperature);
            if delta != 0 {
                out.layers.push((idx, LayerId::Temperature, delta));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use proptest::prelude::*;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, CellAabb, ChunkPos};
    use rmc_world::ledger::SeasonLedger;
    use rmc_world::materials::{MaterialDef, MaterialTable};
    use rmc_world::world::World;

    use crate::runner::{Exec, JacobiReport, run_jacobi};
    use crate::tables::EcoSources;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    /// Materials later features add to `materials.ron` (F120 water and ice,
    /// F130 magma sources), defined here so the clamps can be tested now.
    /// A row whose key the shipped table already has is skipped.
    const EXTRA: [&str; 3] = [
        r#"(id: 0, key: "water", phase: Liquid, tags: [], hardness: 0, diggable: true,
            yield_count: 0, color: (40, 90, 200, 160), emissive: 0.0,
            conductivity: 64, heat_capacity: 8)"#,
        r#"(id: 0, key: "ice", phase: Solid, tags: [], hardness: 5, diggable: true,
            yield_count: 0, color: (200, 230, 255, 255), emissive: 0.0,
            conductivity: 64, heat_capacity: 8)"#,
        r#"(id: 0, key: "test_core", phase: Solid, tags: [], hardness: 0, diggable: false,
            yield_count: 0, color: (255, 80, 0, 255), emissive: 0.0,
            conductivity: 64, heat_capacity: 32, heat_source: Some(13000))"#,
    ];

    fn tables() -> EcoTables {
        let base = MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap();
        let mut defs: Vec<MaterialDef> = base.iter().cloned().collect();
        for text in EXTRA {
            let mut d: MaterialDef = ron::from_str(text).unwrap();
            if base.id(&d.key).is_some() {
                continue;
            }
            d.id = MaterialId(defs.len() as u16);
            defs.push(d);
        }
        let m = Arc::new(MaterialTable::from_defs(defs).unwrap());
        EcoTables::from_sources(m, &EcoSources::read(ASSETS).unwrap()).unwrap()
    }

    struct Fix {
        t: EcoTables,
        air: MaterialId,
        soil: MaterialId,
        dirt: MaterialId,
        stone: MaterialId,
        granite: MaterialId,
        water: MaterialId,
        ice: MaterialId,
        core: MaterialId,
    }

    impl Fix {
        fn new() -> Self {
            let t = tables();
            let id = |k: &str| t.materials().id(k).unwrap();
            Self {
                air: id("air"),
                soil: id("topsoil"),
                dirt: id("dirt"),
                stone: id("stone"),
                granite: id("granite"),
                water: id("water"),
                ice: id("ice"),
                core: id("test_core"),
                t,
            }
        }

        /// A world over `bounds` whose resident chunks are those `resident`
        /// accepts; `cell` gives each in-bounds cell's material and
        /// temperature.
        fn world(
            &self,
            bounds: CellAabb,
            resident: impl Fn(ChunkPos) -> bool,
            cell: impl Fn(CellPos) -> (MaterialId, i16),
        ) -> World {
            let mut w = World::new(self.t.materials().clone(), bounds);
            w.set_eco_rules(self.t.displacement_rules());
            for pos in bounds.chunks().filter(|&p| resident(p)) {
                let mut c = Chunk::uniform(self.air);
                for i in 0..CHUNK_VOLUME as u16 {
                    let l = LocalPos::from_index(i);
                    let at = CellPos::from_chunk_local(pos, l);
                    if bounds.contains(at) {
                        let (m, t) = cell(at);
                        c.storage.set(l, m);
                        c.layers.set_i16(LayerId::Temperature, l, t);
                    }
                }
                w.insert_chunk(pos, c).unwrap();
            }
            w
        }

        /// Random materials (no heat sources) and temperatures under a
        /// stone lid, over two chunks side by side. The chunks below the
        /// bottom cell layer are not resident and every air, water or ice
        /// cell lies under the lid, so no cell is clamped.
        fn unclamped(&self, seed: u64) -> World {
            let bounds = CellAabb::new(CellPos::new(-8, -1, 0), CellPos::new(7, 11, 7));
            let pick = [
                self.air,
                self.soil,
                self.stone,
                self.granite,
                self.water,
                self.ice,
            ];
            self.world(
                bounds,
                |p| p.0.y >= 0,
                |c| {
                    let h = noise(c, seed);
                    let m = if c.0.y == bounds.max.0.y {
                        self.stone
                    } else {
                        pick[(h % pick.len() as u64) as usize]
                    };
                    (m, ((h >> 16) % 4001) as i16 - 2000)
                },
            )
        }
    }

    /// Deterministic per-cell noise.
    fn noise(cell: CellPos, salt: u64) -> u64 {
        let c = cell.0;
        let mut h = salt ^ 0x9E37_79B9_7F4A_7C15;
        for v in [c.x, c.y, c.z] {
            h = (h ^ (v as u32 as u64)).wrapping_mul(0x100_0000_01B3);
            h ^= h >> 29;
        }
        h
    }

    fn step(w: &mut World, pass: &TemperaturePass) -> JacobiReport {
        let mut ledger = SeasonLedger::default();
        let r = run_jacobi(w, pass, Exec::Parallel, &mut ledger);
        assert_eq!(ledger, SeasonLedger::default(), "temperature books nothing");
        r
    }

    fn temp(w: &World, cell: CellPos) -> i32 {
        w.layer(cell, LayerId::Temperature)
    }

    /// Every in-bounds resident cell.
    fn cells(w: &World) -> Vec<CellPos> {
        let b = w.bounds();
        let mut v = Vec::new();
        for y in b.min.0.y..=b.max.0.y {
            for z in b.min.0.z..=b.max.0.z {
                for x in b.min.0.x..=b.max.0.x {
                    let c = CellPos::new(x, y, z);
                    if w.get_cell(c).is_some() {
                        v.push(c);
                    }
                }
            }
        }
        v
    }

    fn capacity(w: &World, c: CellPos) -> i64 {
        let m = w.get_cell(c).unwrap();
        i64::from(w.materials().get(m).unwrap().heat_capacity)
    }

    /// `Σ T·C`.
    fn energy(w: &World) -> i64 {
        cells(w)
            .into_iter()
            .map(|c| i64::from(temp(w, c)) * capacity(w, c))
            .sum()
    }

    fn spread(w: &World) -> i32 {
        let ts: Vec<i32> = cells(w).into_iter().map(|c| temp(w, c)).collect();
        ts.iter().max().unwrap() - ts.iter().min().unwrap()
    }

    fn offset(c: CellPos, d: [i32; 3]) -> CellPos {
        CellPos::new(c.0.x + d[0], c.0.y + d[1], c.0.z + d[2])
    }

    fn cases() -> u32 {
        std::env::var("PROPTEST_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(256)
    }

    #[test]
    fn energy_nonincreasing_without_sources() {
        let f = Fix::new();
        let pass = TemperaturePass::new(&f.t, 0);
        for seed in 0..4 {
            let mut w = f.unclamped(seed);
            let mut e = energy(&w);
            let h0 = w.hash();
            for _ in 0..10 {
                step(&mut w, &pass);
                let next = energy(&w);
                assert!(next <= e, "seed {seed}: energy rose from {e} to {next}");
                assert_eq!(next, e, "seed {seed}: every edge conserves energy");
                e = next;
            }
            assert_ne!(w.hash(), h0, "seed {seed}: heat moved");
        }
    }

    #[test]
    fn unequal_capacity_edge_conserves_energy() {
        let soil = |t| HeatCell { t, k: 64, c: 8 };
        let rock = |t| HeatCell { t, k: 16, c: 32 };
        // SAD-P2 §7.1: f = 12 between soil (C 8) and rock (C 32) moves
        // nothing; truncating per side would give the soil a degree.
        assert_eq!(flux(soil(0), rock(96), 7), 12);
        assert_eq!(edge_delta(soil(0), rock(96), 7), 0);
        assert_eq!(edge_delta(rock(96), soil(0), 7), 0);
        // f = 32 moves one quantum of 32: soil +4, rock -1.
        assert_eq!(edge_delta(soil(0), rock(256), 7), 4);
        assert_eq!(edge_delta(rock(256), soil(0), 7), -1);

        // A soil block against a hot rock block, across a chunk face.
        let f = Fix::new();
        let pass = TemperaturePass::new(&f.t, 0);
        let bounds = CellAabb::new(CellPos::new(-8, -1, 0), CellPos::new(7, 11, 7));
        let mut w = f.world(
            bounds,
            |p| p.0.y >= 0,
            |c| match (c.0.y == bounds.max.0.y, c.0.x < 0) {
                (true, _) => (f.stone, 0),
                (false, true) => (f.soil, 0),
                (false, false) => (f.granite, 3000),
            },
        );
        let e = energy(&w);
        for _ in 0..20 {
            step(&mut w, &pass);
            assert_eq!(energy(&w), e);
        }
        assert!(
            temp(&w, CellPos::new(-1, 4, 4)) > 0,
            "heat crossed the edge"
        );
        assert!(temp(&w, CellPos::new(0, 4, 4)) < 3000);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(cases()))]

        /// No unclamped cell leaves the [min, max] of its own and its six
        /// neighbors' previous temperatures.
        #[test]
        fn maximum_principle_proptest(seed in any::<u64>()) {
            let f = Fix::new();
            let pass = TemperaturePass::new(&f.t, 0);
            let mut w = f.unclamped(seed);
            let before = w.clone();
            step(&mut w, &pass);
            for c in cells(&w) {
                let mut lo = temp(&before, c);
                let mut hi = lo;
                for d in FACES {
                    let n = offset(c, d);
                    if before.get_cell(n).is_some() {
                        lo = lo.min(temp(&before, n));
                        hi = hi.max(temp(&before, n));
                    }
                }
                let t = temp(&w, c);
                prop_assert!(lo <= t && t <= hi, "{:?}: {} outside [{}, {}]", c, t, lo, hi);
            }
        }

        /// `f_ij = −f_ji` and every edge conserves energy, for any pair.
        #[test]
        fn flux_antisymmetric_proptest(
            ti in any::<i16>(), tj in any::<i16>(),
            ki in 0..=64i32, kj in 0..=64i32,
            ci in 3..=7u32, cj in 3..=7u32,
            shift in 0..=7u8,
        ) {
            let i = HeatCell { t: i32::from(ti), k: ki, c: 1 << ci };
            let j = HeatCell { t: i32::from(tj), k: kj, c: 1 << cj };
            prop_assert_eq!(flux(i, j, shift), -flux(j, i, shift));
            prop_assert_eq!(
                i.c * edge_delta(i, j, shift),
                -(j.c * edge_delta(j, i, shift))
            );
        }
    }

    #[test]
    fn spread_nonincreasing() {
        let f = Fix::new();
        let pass = TemperaturePass::new(&f.t, 0);
        for seed in 10..14 {
            let mut w = f.unclamped(seed);
            let mut s = spread(&w);
            for _ in 0..10 {
                step(&mut w, &pass);
                let next = spread(&w);
                assert!(next <= s, "seed {seed}: spread rose from {s} to {next}");
                s = next;
            }
        }
    }

    #[test]
    fn linear_profile_is_fixed_point() {
        let f = Fix::new();
        let mut pass = TemperaturePass::new(&f.t, 0);
        // Steep enough that every vertical edge carries heat.
        pass.profile = Profile {
            geothermal_base: 400,
            gradient: 40,
        };
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(7, 63, 7));
        let top = bounds.max.0.y;
        pass.air_temp = pass.profile.at(0, top);
        // Soil to y 62, open air above: uniform conductivity and capacity.
        let profile = pass.profile;
        let mut w = f.world(
            bounds,
            |_| true,
            |c| {
                let m = if c.0.y == top { f.air } else { f.soil };
                (m, profile.at(0, c.0.y))
            },
        );
        let a = HeatCell {
            t: i32::from(profile.at(0, 10)),
            k: 64,
            c: 8,
        };
        let b = HeatCell {
            t: i32::from(profile.at(0, 11)),
            ..a
        };
        assert_ne!(edge_delta(a, b, pass.heat_shift), 0, "edges carry heat");
        let h = w.hash();
        for _ in 0..20 {
            let r = step(&mut w, &pass);
            assert!(r.changed.is_empty(), "changed {:?}", r.changed);
        }
        assert_eq!(w.hash(), h);
    }

    #[test]
    fn heat_source_stays_clamped() {
        let f = Fix::new();
        let pass = TemperaturePass::new(&f.t, 0);
        let source = f.t.materials().get(f.core).unwrap().heat_source.unwrap();
        let core = CellPos::new(0, 5, 4);
        let mut w = f.world(
            CellAabb::new(CellPos::new(-8, -1, 0), CellPos::new(7, 11, 7)),
            |p| p.0.y >= 0,
            |c| if c == core { (f.core, 0) } else { (f.soil, 0) },
        );
        for _ in 0..20 {
            step(&mut w, &pass);
            assert_eq!(temp(&w, core), i32::from(source));
        }
        for d in FACES {
            assert!(temp(&w, offset(core, d)) > 0, "neighbor {d:?} warmed");
        }
    }

    #[test]
    fn surface_clamp_pond_top_and_open_air_not_under_overhang() {
        let f = Fix::new();
        let pass = TemperaturePass::new(&f.t, 3);
        let air_temp = i32::from(pass.air_temp);
        assert!(air_temp < 0, "winter");
        let pond = CellPos::new(4, 7, 4);
        let ice = CellPos::new(10, 7, 10);
        let ground = CellPos::new(8, 7, 8);
        let open = CellPos::new(8, 12, 8);
        let above_slab = CellPos::new(2, 16, 12);
        let under_slab = CellPos::new(2, 10, 12);
        let slab = CellPos::new(2, 14, 12);
        let mut w = f.world(
            CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(15, 23, 15)),
            |_| true,
            |c| {
                let p = c.0;
                let m = if c == pond {
                    f.water
                } else if c == ice {
                    f.ice
                } else if p.y <= 7 {
                    f.soil
                } else if p.y == 14 && p.x < 5 && (10..15).contains(&p.z) {
                    f.stone
                } else {
                    f.air
                };
                (m, 500)
            },
        );
        assert_eq!(w.surface_y(under_slab.0.x, under_slab.0.z), 14);
        step(&mut w, &pass);
        for c in [pond, ice, open, above_slab] {
            assert_eq!(temp(&w, c), air_temp, "{c:?} is clamped");
        }
        assert_eq!(temp(&w, under_slab), 500, "air under the overhang is not");
        for c in [ground, slab] {
            let t = temp(&w, c);
            assert!(
                air_temp < t && t < 500,
                "solid surface {c:?} cools, not clamped: {t}"
            );
        }
    }

    /// Soil over stone, surface at y 60, a one-cell pond at the surface;
    /// temperatures as worldgen writes them (`profile` below the surface,
    /// season 0's `air_temp` at and above it).
    fn pond_world(f: &Fix) -> (World, CellPos, CellPos) {
        let profile = Profile::new(f.t.params());
        let air_temp = TemperaturePass::new(&f.t, 0).air_temp;
        let surface = 60;
        let pond = CellPos::new(8, surface, 8);
        let w = f.world(
            CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(15, 71, 15)),
            |_| true,
            |c| {
                let y = c.0.y;
                let m = if c == pond {
                    f.water
                } else if y > surface {
                    f.air
                } else if y >= surface - 1 {
                    f.soil
                } else if y >= surface - 6 {
                    f.dirt
                } else {
                    f.stone
                };
                let t = if y >= surface {
                    air_temp
                } else {
                    profile.at(0, y)
                };
                (m, t)
            },
        );
        (w, pond, offset(pond, [0, -1, 0]))
    }

    /// Runs `season`'s pass for one season of sub-steps.
    fn season(f: &Fix, w: &mut World, season: u32) {
        let pass = TemperaturePass::new(&f.t, season);
        for _ in 0..f.t.seasons().substeps {
            step(w, &pass);
        }
    }

    fn season_index(f: &Fix, key: &str) -> u32 {
        f.t.seasons()
            .seasons
            .iter()
            .position(|s| s.key == key)
            .unwrap() as u32
    }

    #[test]
    fn surface_cell_follows_seasonal_swing() {
        let f = Fix::new();
        let (mut w, pond, floor) = pond_world(&f);
        let (winter, summer) = (season_index(&f, "winter"), season_index(&f, "summer"));
        // The pond is clamped, so it crosses 0 °C with every season. Its
        // floor gains at most 1/16 of the gap to the pond per sub-step and
        // the ground below stays warm, so from the worldgen profile it
        // freezes only in a run of winters; one summer then thaws it.
        const MAX_WINTERS: usize = 8;
        assert!(temp(&w, pond) > 0 && temp(&w, floor) > 0);
        let mut n = 0;
        loop {
            season(&f, &mut w, winter);
            n += 1;
            assert!(temp(&w, pond) < 0, "the pond freezes every winter");
            if temp(&w, floor) < 0 {
                break;
            }
            assert!(
                n < MAX_WINTERS,
                "floor {} after {n} winters",
                temp(&w, floor)
            );
        }
        season(&f, &mut w, summer);
        assert!(temp(&w, pond) > 0, "pond {}", temp(&w, pond));
        assert!(
            temp(&w, floor) > 0,
            "floor {} after one summer",
            temp(&w, floor)
        );
        season(&f, &mut w, winter);
        assert!(temp(&w, pond) < 0, "pond {}", temp(&w, pond));
    }

    #[test]
    fn deep_profile_cell_quiet_20_substeps() {
        let f = Fix::new();
        let (mut w, _, _) = pond_world(&f);
        let profile = Profile::new(f.t.params());
        let pass = TemperaturePass::new(&f.t, 0);
        let deep = CellPos::new(8, 20, 8);
        let bottom = ChunkPos::new(0, 0, 0);
        for _ in 0..20 {
            let r = step(&mut w, &pass);
            assert_eq!(temp(&w, deep), i32::from(profile.at(0, 20)));
            assert!(!r.changed.contains(&bottom), "the deep chunk is quiet");
        }
    }

    #[test]
    fn bottom_layer_clamped_to_profile() {
        let f = Fix::new();
        let pass = TemperaturePass::new(&f.t, 0);
        let bounds = CellAabb::new(CellPos::new(-8, -4, 0), CellPos::new(7, 8, 7));
        let bottom_y = bounds.min.0.y;
        let want = i32::from(pass.profile.at(bottom_y, bottom_y));
        assert_ne!(want, 0);
        // Everything starts at 0, under a lid so nothing else is clamped.
        let mut w = f.world(
            bounds,
            |_| true,
            |c| {
                let m = if c.0.y == bounds.max.0.y {
                    f.stone
                } else {
                    f.soil
                };
                (m, 0)
            },
        );
        for _ in 0..4 {
            step(&mut w, &pass);
            for c in cells(&w).into_iter().filter(|c| c.0.y == bottom_y) {
                assert_eq!(temp(&w, c), want, "{c:?}");
            }
        }
        assert!(
            temp(&w, CellPos::new(0, bottom_y + 1, 4)) > 0,
            "the floor heats"
        );
    }

    #[test]
    fn two_chunk_equals_one_chunk() {
        let f = Fix::new();
        // The same 32³ content inside one chunk, and shifted by 16 cells on
        // every axis so it straddles 8 chunks.
        let content = |rel: [i32; 3]| {
            let c = CellPos::new(rel[0], rel[1], rel[2]);
            let h = noise(c, 7);
            let ground = 18 + (noise(CellPos::new(rel[0], 0, rel[2]), 3) % 6) as i32;
            let m = if rel[1] == 26 && rel[0] < 12 && rel[2] < 12 {
                f.stone
            } else if rel[1] > ground {
                f.air
            } else if rel[1] == ground && h.is_multiple_of(7) {
                f.water
            } else if h.is_multiple_of(61) {
                f.core
            } else {
                [f.soil, f.stone, f.granite, f.air][(h % 4) as usize]
            };
            (m, ((h >> 20) % 3001) as i16 - 1500)
        };
        let make = |shift: i32| {
            let lo = CellPos::new(shift, shift, shift);
            let hi = CellPos::new(shift + 31, shift + 31, shift + 31);
            f.world(
                CellAabb::new(lo, hi),
                |_| true,
                |c| content([c.0.x - shift, c.0.y - shift, c.0.z - shift]),
            )
        };
        let mut one = make(0);
        let mut eight = make(16);
        assert_eq!(one.chunk_count(), 1);
        assert_eq!(eight.chunk_count(), 8);
        let pass = TemperaturePass::new(&f.t, 3);
        for _ in 0..4 {
            run_jacobi(&mut one, &pass, Exec::Serial, &mut SeasonLedger::default());
            run_jacobi(
                &mut eight,
                &pass,
                Exec::Parallel,
                &mut SeasonLedger::default(),
            );
            for c in cells(&one) {
                let s = offset(c, [16, 16, 16]);
                assert_eq!(temp(&one, c), temp(&eight, s), "{c:?}");
            }
        }
    }
}
