//! Light (SAD-P2 §7.7): a global, single-threaded sweep, recomputed only
//! when the world's light dirty flag is set (any chunk's `revision`
//! changed since the last light pass).
//!
//! The light of a cell is a pure function of the materials:
//!
//! 1. **Column pass**, y descending per column: light is 255 above the
//!    world, and `light.saturating_sub(light_attenuation)` entering each
//!    cell (`MaterialDef::light_attenuation`: air, passable solids and ice
//!    0, water 32, other solids and magma 255).
//! 2. **Lateral**: `eco.light_lateral_iters` double-buffered iterations over
//!    the cells with `light_attenuation < 255`: `light = max(light,
//!    max_neighbor.saturating_sub(eco.light_falloff))` over the six face
//!    neighbors, every value computed from the previous iteration's array.
//!
//! The lateral step visits only the cells that can change: in the first
//! iteration the transparent cells below 255, and in each later one the
//! transparent neighbors of the cells the previous iteration raised. A cell
//! none of whose neighbors changed keeps its value, so the result equals
//! the full double-buffered iteration (`lateral_falloff` checks it against
//! one).

use rmc_world::chunk::Chunk;
use rmc_world::coords::{CHUNK_SIZE, CHUNK_VOLUME, CellAabb, CellPos, ChunkPos, LocalPos};
use rmc_world::world::World;

use crate::tables::EcoTables;

/// Light above the world.
const FULL: u8 = u8::MAX;
/// Attenuation of a cell that blocks light; the lateral step skips it.
const OPAQUE: u8 = u8::MAX;

/// The light pass, built once from the tables.
#[derive(Clone, Debug)]
pub struct LightPass {
    lateral_iters: u8,
    falloff: u8,
    /// `light_attenuation` by material id.
    attenuation: Vec<u8>,
}

/// The light of every cell in a box, in (y, z, x) order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightGrid {
    bounds: CellAabb,
    dims: Dims,
    data: Vec<u8>,
}

/// Extent of a box and its flat (y, z, x) indexing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Dims {
    min: CellPos,
    nx: usize,
    ny: usize,
    nz: usize,
}

impl Dims {
    fn of(bounds: CellAabb) -> Self {
        let size = |lo: i32, hi: i32| usize::try_from(hi - lo + 1).expect("min ≤ max");
        Self {
            min: bounds.min,
            nx: size(bounds.min.0.x, bounds.max.0.x),
            ny: size(bounds.min.0.y, bounds.max.0.y),
            nz: size(bounds.min.0.z, bounds.max.0.z),
        }
    }

    fn len(self) -> usize {
        self.nx * self.ny * self.nz
    }

    /// Flat index of `cell`, or `None` outside the box.
    fn index(self, cell: CellPos) -> Option<usize> {
        let d = cell.0 - self.min.0;
        let (x, y, z) = (
            usize::try_from(d.x).ok()?,
            usize::try_from(d.y).ok()?,
            usize::try_from(d.z).ok()?,
        );
        (x < self.nx && y < self.ny && z < self.nz).then(|| (y * self.nz + z) * self.nx + x)
    }

    /// The face neighbors of flat index `i` inside the box.
    fn neighbors(self, i: usize) -> impl Iterator<Item = usize> {
        let (nx, nz) = (self.nx, self.nz);
        let plane = nx * nz;
        let (x, z, y) = (i % nx, (i / nx) % nz, i / plane);
        [
            (x > 0).then(|| i - 1),
            (x + 1 < nx).then(|| i + 1),
            (z > 0).then(|| i - nx),
            (z + 1 < nz).then(|| i + nx),
            (y > 0).then(|| i - plane),
            (y + 1 < self.ny).then(|| i + plane),
        ]
        .into_iter()
        .flatten()
    }
}

impl LightGrid {
    /// The box the grid covers.
    pub fn bounds(&self) -> CellAabb {
        self.bounds
    }

    /// Light at `cell`; 0 outside the box.
    pub fn get(&self, cell: CellPos) -> u8 {
        self.dims.index(cell).map_or(0, |i| self.data[i])
    }

    /// The `Light` layer of the chunk at `pos`, indexed by
    /// [`LocalPos::index`]; cells outside the box are 0.
    pub fn chunk_values(&self, pos: ChunkPos) -> Box<[u8; CHUNK_VOLUME]> {
        let mut out: Box<[u8; CHUNK_VOLUME]> = vec![0; CHUNK_VOLUME]
            .into_boxed_slice()
            .try_into()
            .expect("CHUNK_VOLUME entries");
        let n = CHUNK_SIZE as u32;
        for ly in 0..n {
            for lz in 0..n {
                for lx in 0..n {
                    let l = LocalPos::new(lx, ly, lz);
                    if let Some(i) = self.dims.index(CellPos::from_chunk_local(pos, l)) {
                        out[usize::from(l.index())] = self.data[i];
                    }
                }
            }
        }
        out
    }
}

impl LightPass {
    pub fn new(tables: &EcoTables) -> Self {
        let p = tables.params();
        Self {
            lateral_iters: p.light_lateral_iters,
            falloff: p.light_falloff,
            attenuation: tables
                .materials()
                .iter()
                .map(|d| d.light_attenuation())
                .collect(),
        }
    }

    /// Recomputes the world's light if its dirty flag is set, and returns
    /// whether it did.
    pub fn run(&self, world: &mut World) -> bool {
        if !world.light_dirty() {
            return false;
        }
        self.recompute(world);
        true
    }

    /// Recomputes the world's light, writes each resident chunk's `Light`
    /// layer in sorted chunk order and clears the dirty flag.
    pub fn recompute(&self, world: &mut World) {
        let grid = self.compute(world.bounds(), world.iter_chunks_sorted());
        let positions: Vec<ChunkPos> = world.iter_chunks_sorted().map(|(p, _)| p).collect();
        let mut changed = 0usize;
        for pos in positions {
            changed += usize::from(world.eco_put_light(pos, &grid.chunk_values(pos)));
        }
        world.eco_clear_light_dirty();
        tracing::debug!(changed, "light recomputed");
    }

    /// The light of every cell in `bounds`, from the materials of `chunks`.
    /// A cell of `bounds` in no given chunk blocks light.
    pub fn compute<'a>(
        &self,
        bounds: CellAabb,
        chunks: impl IntoIterator<Item = (ChunkPos, &'a Chunk)>,
    ) -> LightGrid {
        let dims = Dims::of(bounds);
        let attenuation = self.attenuation_grid(dims, chunks);
        let mut data = column_pass(dims, &attenuation);
        self.lateral(dims, &attenuation, &mut data);
        LightGrid { bounds, dims, data }
    }

    /// `light_attenuation` of every cell in the box; [`OPAQUE`] where no
    /// chunk is given. Each chunk writes only its own cells, so the order of
    /// `chunks` does not matter.
    fn attenuation_grid<'a>(
        &self,
        dims: Dims,
        chunks: impl IntoIterator<Item = (ChunkPos, &'a Chunk)>,
    ) -> Vec<u8> {
        let mut out = vec![OPAQUE; dims.len()];
        let n = CHUNK_SIZE as u32;
        for (pos, chunk) in chunks {
            for ly in 0..n {
                for lz in 0..n {
                    for lx in 0..n {
                        let l = LocalPos::new(lx, ly, lz);
                        if let Some(i) = dims.index(CellPos::from_chunk_local(pos, l)) {
                            let m = chunk.storage.get(l);
                            out[i] = self
                                .attenuation
                                .get(usize::from(m.0))
                                .copied()
                                .unwrap_or(OPAQUE);
                        }
                    }
                }
            }
        }
        out
    }

    /// The lateral step over the column pass's `light`.
    fn lateral(&self, dims: Dims, attenuation: &[u8], light: &mut [u8]) {
        let can_rise = |i: usize, light: &[u8]| attenuation[i] < OPAQUE && light[i] < FULL;
        let mut candidates: Vec<usize> = (0..light.len()).filter(|&i| can_rise(i, light)).collect();
        for _ in 0..self.lateral_iters {
            // Computed from the previous iteration's array, then applied.
            let raised: Vec<(usize, u8)> = candidates
                .iter()
                .filter_map(|&i| {
                    let best = dims.neighbors(i).map(|n| light[n]).max()?;
                    let v = best.saturating_sub(self.falloff);
                    (v > light[i]).then_some((i, v))
                })
                .collect();
            if raised.is_empty() {
                break;
            }
            for &(i, v) in &raised {
                light[i] = v;
            }
            candidates = raised
                .iter()
                .flat_map(|&(i, _)| dims.neighbors(i))
                .filter(|&n| can_rise(n, light))
                .collect();
            candidates.sort_unstable();
            candidates.dedup();
        }
    }
}

/// Step 1: each column from the top of the box down.
fn column_pass(dims: Dims, attenuation: &[u8]) -> Vec<u8> {
    let mut light = vec![0; dims.len()];
    let plane = dims.nx * dims.nz;
    for col in 0..plane {
        let mut l = FULL;
        for y in (0..dims.ny).rev() {
            let i = y * plane + col;
            l = l.saturating_sub(attenuation[i]);
            light[i] = l;
        }
    }
    light
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use rmc_world::layers::LayerId;
    use rmc_world::ledger::SeasonLedger;
    use rmc_world::materials::{MaterialDef, MaterialId, MaterialTable};

    use crate::activity::Activity;
    use crate::runner::Exec;
    use crate::tables::SeasonOverride;
    use crate::tick::season_tick;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    /// The shipped materials plus a passable plant-like solid with no
    /// `light_attenuation` of its own (the shipped table has no plants yet).
    fn tables() -> EcoTables {
        let base = MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap();
        let mut defs: Vec<MaterialDef> = base.iter().cloned().collect();
        let mut d: MaterialDef = ron::from_str(
            r#"(id: 0, key: "test_plant", phase: Solid, tags: ["plant"], passable: true,
                hardness: 1, diggable: true, yield_count: 1, color: (60, 160, 60, 255),
                emissive: 0.0)"#,
        )
        .unwrap();
        d.id = MaterialId(defs.len() as u16);
        defs.push(d);
        let m = Arc::new(MaterialTable::from_defs(defs).unwrap());
        EcoTables::load_with_materials(m, ASSETS).unwrap()
    }

    fn id(t: &EcoTables, key: &str) -> MaterialId {
        t.materials().id(key).unwrap()
    }

    /// One chunk's worth of world, 32 × 16 × 32 cells, with `at(cell)` as
    /// each cell's material.
    fn world(t: &EcoTables, at: impl Fn(CellPos) -> MaterialId) -> World {
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 15, 31));
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        w.set_eco_rules(t.displacement_rules());
        for pos in bounds.chunks() {
            let mut c = Chunk::uniform(MaterialId::AIR);
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                let cell = CellPos::from_chunk_local(pos, l);
                if bounds.contains(cell) {
                    c.storage.set(l, at(cell));
                }
            }
            w.insert_chunk(pos, c).unwrap();
        }
        w
    }

    fn light(w: &World, cell: CellPos) -> i32 {
        w.layer(cell, LayerId::Light)
    }

    /// Stone floor at y ≤ 2; a stone roof at y = 9 over x ≥ 8 (the whole
    /// z range), air everywhere else.
    fn overhang(t: &EcoTables) -> World {
        let stone = id(t, "stone");
        world(t, |c| {
            let (x, y) = (c.0.x, c.0.y);
            if y <= 2 || (y == 9 && x >= 8) {
                stone
            } else {
                MaterialId::AIR
            }
        })
    }

    /// The full double-buffered lateral iteration over every transparent
    /// cell, straight from SAD-P2 §7.7.
    fn reference(t: &EcoTables, w: &World) -> LightGrid {
        let pass = LightPass::new(t);
        let dims = Dims::of(w.bounds());
        let att = pass.attenuation_grid(dims, w.iter_chunks_sorted());
        let mut prev = column_pass(dims, &att);
        for _ in 0..pass.lateral_iters {
            let mut next = prev.clone();
            for i in 0..prev.len() {
                if att[i] < OPAQUE {
                    let best = dims.neighbors(i).map(|n| prev[n]).max().unwrap_or(0);
                    next[i] = next[i].max(best.saturating_sub(pass.falloff));
                }
            }
            prev = next;
        }
        LightGrid {
            bounds: w.bounds(),
            dims,
            data: prev,
        }
    }

    #[test]
    fn open_column_full_overhang_zero() {
        let t = tables();
        let p = t.params();
        let mut w = overhang(&t);
        LightPass::new(&t).recompute(&mut w);
        // The lateral step reaches this many cells in from the open edge.
        let reach =
            i32::from(p.light_lateral_iters).min(i32::from(u8::MAX / p.light_falloff.max(1)));
        for z in [0, 13, 31] {
            for y in 0..=15 {
                // An open column: 255 down to the floor, 0 in it.
                let want = if y <= 2 { 0 } else { 255 };
                assert_eq!(light(&w, CellPos::new(2, y, z)), want, "open (2, {y}, {z})");
                // Under the roof, out of the lateral step's reach: 0 under
                // the roof and in it, 255 above it.
                for x in 8 + reach..=31 {
                    let want = if y >= 10 { 255 } else { 0 };
                    assert_eq!(light(&w, CellPos::new(x, y, z)), want, "({x}, {y}, {z})");
                }
            }
        }
        // Every chunk carries the layer.
        for (pos, c) in w.iter_chunks_sorted() {
            assert!(c.layers.contains(LayerId::Light), "{pos:?}");
        }
    }

    #[test]
    fn lateral_falloff() {
        let t = tables();
        let p = t.params();
        let (iters, falloff) = (i32::from(p.light_lateral_iters), i32::from(p.light_falloff));
        assert!(iters > 0 && falloff > 0, "the shipped params spread light");
        let mut w = overhang(&t);
        LightPass::new(&t).recompute(&mut w);
        // x = 7 is the last open column; k cells in under the roof the light
        // is 255 − k · falloff, out to `iters` cells, then 0.
        for y in 3..=8 {
            for k in 1..=iters + 2 {
                let want = if k <= iters {
                    (255 - k * falloff).max(0)
                } else {
                    0
                };
                assert_eq!(light(&w, CellPos::new(7 + k, y, 5)), want, "k {k}, y {y}");
            }
        }

        // The lateral step also climbs: a notch cut up into the roof two
        // cells in from the edge is lit only from below, one falloff less
        // than the cell under it.
        let stone = id(&t, "stone");
        let mut w = world(&t, |c| {
            let (x, y, z) = (c.0.x, c.0.y, c.0.z);
            let notch = (x, y, z) == (9, 9, 5);
            if y <= 2 || ((9..=11).contains(&y) && x >= 8 && !notch) {
                stone
            } else {
                MaterialId::AIR
            }
        });
        LightPass::new(&t).recompute(&mut w);
        let under = light(&w, CellPos::new(9, 8, 5));
        assert_eq!(under, 255 - 2 * falloff);
        assert_eq!(light(&w, CellPos::new(9, 9, 5)), under - falloff);

        // On scattered caves, pillars, water and ice, the pass equals the
        // full double-buffered iteration.
        let mats = [
            MaterialId::AIR,
            MaterialId::AIR,
            MaterialId::AIR,
            stone,
            stone,
            id(&t, "water"),
            id(&t, "ice"),
            id(&t, "test_plant"),
        ];
        for seed in 0..8u64 {
            let mut w = world(&t, |c| {
                let mut h = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                for v in [c.0.x, c.0.y, c.0.z] {
                    h = (h ^ v as u64).wrapping_mul(0x100_0000_01B3);
                    h ^= h >> 29;
                }
                mats[(h % mats.len() as u64) as usize]
            });
            let want = reference(&t, &w);
            LightPass::new(&t).recompute(&mut w);
            let got = LightPass::new(&t).compute(w.bounds(), w.iter_chunks_sorted());
            assert_eq!(got, want, "seed {seed}");
            for pos in w.bounds().chunks() {
                let c = w.chunk(pos).unwrap();
                let Some(rmc_world::layers::LayerData::U8(layer)) = c.layers.data(LayerId::Light)
                else {
                    panic!("{pos:?} has a u8 light layer");
                };
                assert_eq!(**layer, *want.chunk_values(pos), "seed {seed}, {pos:?}");
            }
        }
    }

    #[test]
    fn water_attenuates_passable_transparent() {
        let t = tables();
        let m = t.materials();
        let (stone, water, ice, plant) = (
            id(&t, "stone"),
            id(&t, "water"),
            id(&t, "ice"),
            id(&t, "test_plant"),
        );
        let att = |k| m.get(k).unwrap().light_attenuation();
        assert_eq!(att(water), 32, "the table sets water");
        assert_eq!(att(ice), 0, "the table sets ice");
        assert_eq!(att(plant), 0, "a passable solid defaults to 0");
        assert_eq!(att(stone), 255, "other solids default to 255");
        // Every column: plant, ice, plant, air, a pool, the floor. Whole
        // layers, so no lateral light enters from the side.
        let mut w = world(&t, |c| match c.0.y {
            14 | 12 => plant,
            13 => ice,
            8..=11 | 15 => MaterialId::AIR,
            3..=7 => water,
            _ => stone,
        });
        LightPass::new(&t).recompute(&mut w);
        for (x, z) in [(0, 0), (9, 21), (31, 31)] {
            for y in 8..=15 {
                assert_eq!(light(&w, CellPos::new(x, y, z)), 255, "({x}, {y}, {z})");
            }
            for y in 3..=7 {
                let depth = 8 - y;
                assert_eq!(
                    light(&w, CellPos::new(x, y, z)),
                    255 - depth * i32::from(att(water)),
                    "water at depth {depth}"
                );
            }
            for y in 0..=2 {
                assert_eq!(light(&w, CellPos::new(x, y, z)), 0);
            }
        }
        // Light passes the plant into the pool as it would open air.
        let mut open = world(&t, |c| match c.0.y {
            3..=7 => water,
            0..=2 => stone,
            _ => MaterialId::AIR,
        });
        LightPass::new(&t).recompute(&mut open);
        for (pos, c) in w.iter_chunks_sorted() {
            assert_eq!(
                c.layers.data(LayerId::Light),
                open.chunk(pos).unwrap().layers.data(LayerId::Light),
                "{pos:?}"
            );
        }
    }

    #[test]
    fn recomputed_only_when_dirty() {
        let t = tables();
        let pass = LightPass::new(&t);
        let stone = id(&t, "stone");
        let mut w = overhang(&t);
        let roof = CellPos::new(20, 9, 5);
        let under = CellPos::new(20, 5, 5);

        // A new world is dirty; once lit, it is not.
        assert!(w.light_dirty());
        assert!(pass.run(&mut w));
        assert!(!w.light_dirty());
        assert_eq!(light(&w, under), 0);
        assert!(!pass.run(&mut w), "nothing changed");
        // A layer change is not a material change.
        let mut ledger = SeasonLedger::default();
        assert!(w.eco_put_layer(under, LayerId::Moisture, 9));
        assert!(!pass.run(&mut w), "layers do not dirty the light");
        // Setting a cell to what it holds changes nothing.
        w.eco_set(roof, stone, &mut ledger);
        assert!(!pass.run(&mut w), "an unchanged cell");
        // Opening the roof does, and lights the cell under it.
        let eco_rev = w.chunk(under.chunk()).unwrap().eco_revision;
        w.eco_set(roof, MaterialId::AIR, &mut ledger);
        assert!(w.light_dirty());
        assert!(pass.run(&mut w));
        assert_eq!(light(&w, under), 255);
        assert!(w.chunk(under.chunk()).unwrap().eco_revision > eco_rev);
        assert!(!pass.run(&mut w));
        // Inserting a chunk dirties it too.
        let c = w.chunk(under.chunk()).unwrap().clone();
        w.insert_chunk(under.chunk(), c).unwrap();
        assert!(pass.run(&mut w));
        // An unchanged recompute writes nothing.
        let before = w.chunk(under.chunk()).unwrap().eco_revision;
        pass.recompute(&mut w);
        assert_eq!(w.chunk(under.chunk()).unwrap().eco_revision, before);

        // In the season tick: a world whose cells never change (no rain, a
        // fixed air temperature, stone and air) recomputes once, for the
        // insertion, and never in the next season.
        let quiet = t.with_season_override(SeasonOverride {
            rain: Some(0),
            air_temp: Some(150),
        });
        let mut w = overhang(&quiet);
        let mut activity = Activity::new(quiet.params().quiet_skip);
        let r = season_tick(&mut w, &quiet, 0, 1, &mut activity, Exec::Serial);
        assert_eq!(r.light_recomputes, 1, "the first sub-step");
        assert_eq!(light(&w, CellPos::new(2, 5, 5)), 255, "lit by the tick");
        let r = season_tick(&mut w, &quiet, 1, 1, &mut activity, Exec::Serial);
        assert_eq!(r.light_recomputes, 0, "nothing changed");
        // An edit between seasons: one recompute, in the first sub-step.
        w.eco_set(roof, MaterialId::AIR, &mut ledger);
        let r = season_tick(&mut w, &quiet, 2, 1, &mut activity, Exec::Serial);
        assert_eq!(r.light_recomputes, 1);
        assert_eq!(light(&w, under), 255);
    }
}
