//! Soil that can feed plants (`docs/VEGETATION.md` V1).
//!
//! **Horizon.** On gentle ground (no face neighbor's surface more than
//! `max_slope` cells off) where the mask noise passes, the top
//! `min_cells..=max_cells` soil cells of the column become the horizon's
//! porous material, at its starting moisture. Steep ground and the gaps of
//! the mask keep the soil layers as step 2 laid them.
//!
//! **Fertility.** Each column's fertility, in 0..=256, is the mean of its
//! noise and of how low its surface lies (0 on the highest possible
//! surface, 256 on the lowest). Every soil cell of a listed material at or
//! under the surface starts with `lo + (hi - lo) · fertility / 256` of each
//! listed substance.
//!
//! Both read only the column and its neighbors' surface heights, so a
//! chunk is filled alone, after step 6 and before the water table.

use rmc_eco::EcoTables;
use rmc_world::chunk::Chunk;
use rmc_world::coords::{CHUNK_SIZE, ChunkPos, LocalPos};
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;

use crate::r#gen::{SOIL_TAG, resolve};
use crate::noise::{LAYER_FERTILITY, LAYER_HORIZON, LAYER_HORIZON_MASK, NoiseLayer};
use crate::params::{GenError, GenParams};

/// Fertility runs over `0..=FULL`, the fixed divisor of the amount
/// formula.
const FULL: i32 = 256;
/// Half of [`FULL`]: the noise's midpoint, and the height term of a flat
/// world.
const HALF: i32 = FULL / 2;

/// The four face neighbors of a column.
const SIDES: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];

struct Horizon {
    material: MaterialId,
    moisture: u8,
    min_cells: i32,
    max_cells: i32,
    max_slope: i32,
    mask_threshold: f32,
    noise: NoiseLayer,
    mask: NoiseLayer,
}

struct Fertility {
    noise: NoiseLayer,
    /// Per material id: `(substance index, lo, hi)`.
    amounts: Vec<Vec<(u8, i32, i32)>>,
    /// y of the highest possible surface.
    top: i32,
    /// Cells from the highest to the lowest possible surface.
    span: i32,
}

/// Lays the soil horizon and seeds soil nutrients (`docs/VEGETATION.md`
/// V1), from [`GenParams::soil_horizon`] and [`GenParams::fertility`].
pub struct SoilInit {
    horizon: Option<Horizon>,
    fertility: Option<Fertility>,
    /// Per material id: whether it carries the `soil` tag.
    soil: Vec<bool>,
}

impl SoilInit {
    /// From the params and the eco tables, whose material table must be
    /// the one worldgen generates with. Fails on a material that is
    /// missing or lacks the `soil` tag, or a substance the tables lack.
    pub fn new(seed: u64, params: &GenParams, tables: &EcoTables) -> Result<Self, GenError> {
        params.validate()?;
        let materials = tables.materials();
        let horizon = match &params.soil_horizon {
            Some(h) => {
                let material =
                    resolve(materials, "soil_horizon".to_owned(), &h.material, SOIL_TAG)?;
                let capacity = materials.get(material).map_or(0, |d| d.field_capacity);
                Some(Horizon {
                    material,
                    moisture: h.moisture.min(capacity),
                    min_cells: h.min_cells,
                    max_cells: h.max_cells,
                    max_slope: h.max_slope,
                    mask_threshold: h.mask_threshold,
                    noise: NoiseLayer::new(seed, LAYER_HORIZON, &h.noise),
                    mask: NoiseLayer::new(seed, LAYER_HORIZON_MASK, &h.mask),
                })
            }
            None => None,
        };
        let fertility = match &params.fertility {
            Some(f) => {
                let mut amounts = vec![Vec::new(); materials.len()];
                for (i, s) in f.soils.iter().enumerate() {
                    let field = format!("fertility.soils[{i}]");
                    let m = resolve(materials, field.clone(), &s.material, SOIL_TAG)?;
                    amounts[usize::from(m.0)] =
                        s.substances
                            .iter()
                            .map(|(key, lo, hi)| {
                                let x = tables.substance_index(key).ok_or_else(|| {
                                    GenError::Invalid {
                                        field: field.clone(),
                                        reason: format!("substance {key:?} is not in the tables"),
                                    }
                                })?;
                                Ok((x, i32::from(*lo), i32::from(*hi)))
                            })
                            .collect::<Result<_, GenError>>()?;
                }
                let s = &params.surface;
                Some(Fertility {
                    noise: NoiseLayer::new(seed, LAYER_FERTILITY, &f.noise),
                    amounts,
                    top: s.base_height + s.amplitude,
                    span: 2 * s.amplitude,
                })
            }
            None => None,
        };
        Ok(Self {
            horizon,
            fertility,
            soil: materials.iter().map(|d| d.has_tag(SOIL_TAG)).collect(),
        })
    }

    fn is_soil(&self, m: MaterialId) -> bool {
        self.soil.get(usize::from(m.0)).copied().unwrap_or(false)
    }

    /// Cells of the horizon in column `(x, z)`: 0 where the ground is
    /// steep, the mask fails, or there is no horizon.
    pub fn horizon_cells(&self, x: i32, z: i32, surface: impl Fn(i32, i32) -> i32) -> i32 {
        let Some(h) = &self.horizon else {
            return 0;
        };
        let s = surface(x, z);
        let gentle = SIDES
            .iter()
            .all(|&(dx, dz)| (surface(x + dx, z + dz) - s).abs() <= h.max_slope);
        if !gentle || h.mask.sample_2d(x, z) <= h.mask_threshold {
            return 0;
        }
        h.noise.pick_2d(x, z, h.min_cells, h.max_cells)
    }

    /// Fertility of column `(x, z)` with its surface at `surface`, in
    /// 0..=256; 0 without fertility params.
    pub fn fertility(&self, x: i32, z: i32, surface: i32) -> i32 {
        let Some(f) = &self.fertility else {
            return 0;
        };
        let n = (((f.noise.sample_2d(x, z) + 1.0) * HALF as f32).floor() as i32).clamp(0, FULL);
        let low = if f.span == 0 {
            HALF
        } else {
            (f.top - surface).clamp(0, f.span) * FULL / f.span
        };
        (n + low) / 2
    }

    /// The `(substance index, amount)` a cell of `m` starts with at
    /// `fertility`.
    pub fn amounts(&self, m: MaterialId, fertility: i32) -> impl Iterator<Item = (u8, u8)> + '_ {
        self.fertility
            .iter()
            .flat_map(move |f| f.amounts.get(usize::from(m.0)).into_iter().flatten())
            .map(move |&(x, lo, hi)| (x, (lo + (hi - lo) * fertility / FULL) as u8))
    }

    /// Lays the horizon and seeds the nutrients of the generated chunk at
    /// `pos`. `surface(x, z)` is the column's topmost solid cell after
    /// step 1.
    pub fn fill(&self, chunk: &mut Chunk, pos: ChunkPos, surface: impl Fn(i32, i32) -> i32) {
        if self.horizon.is_none() && self.fertility.is_none() {
            return;
        }
        let origin = pos.origin_cell().0;
        let mut laid = false;
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let (cx, cz) = (origin.x + x, origin.z + z);
                let top = surface(cx, cz);
                if top < origin.y {
                    continue;
                }
                let horizon = self.horizon_cells(cx, cz, &surface);
                let fertility = self.fertility(cx, cz, top);
                for y in 0..=(top - origin.y).min(CHUNK_SIZE - 1) {
                    let l = LocalPos::new(x as u32, y as u32, z as u32);
                    let mut m = chunk.storage.get(l);
                    if !self.is_soil(m) {
                        continue;
                    }
                    if let Some(h) = &self.horizon
                        && top - (origin.y + y) < horizon
                    {
                        chunk.storage.set(l, h.material);
                        chunk.layers.set_u8(LayerId::Moisture, l, h.moisture);
                        m = h.material;
                        laid = true;
                    }
                    for (s, amount) in self.amounts(m, fertility) {
                        chunk.layers.set_u8(LayerId::Substance(s), l, amount);
                    }
                }
            }
        }
        if laid {
            chunk.storage.compact();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use rmc_world::coords::CellPos;
    use rmc_world::materials::MaterialTable;

    use crate::Generator;
    use crate::params::WorldBounds;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    const SEED: u64 = 11;

    fn tables() -> EcoTables {
        let m = Arc::new(MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap());
        EcoTables::load_with_materials(m, ASSETS).unwrap()
    }

    /// The shipped params on a 64 × 128 × 64 world.
    fn params() -> GenParams {
        let mut p = GenParams::load(format!("{ASSETS}/worldgen.ron")).unwrap();
        p.bounds = WorldBounds {
            min: [0, 0, 0],
            max: [63, 127, 63],
        };
        p
    }

    /// Every column of the world `g` generates: `(x, z, surface, column
    /// chunks)`, read through `cell`.
    struct Columns {
        chunks: std::collections::BTreeMap<[i32; 3], Chunk>,
    }

    impl Columns {
        fn new(g: &Generator) -> Self {
            Self {
                chunks: g
                    .bounds()
                    .chunks()
                    .map(|pos| (pos.0.to_array(), g.generate(pos)))
                    .collect(),
            }
        }

        fn cell(&self, c: CellPos) -> (MaterialId, &Chunk, LocalPos) {
            let (pos, l) = c.split();
            let chunk = &self.chunks[&pos.0.to_array()];
            (chunk.storage.get(l), chunk, l)
        }
    }

    #[test]
    fn loam_lies_damp_on_gentle_ground_only() {
        let t = tables();
        let mut p = params();
        // The shipped ground is gentle almost everywhere; a slope of 0
        // makes every step steep, so both kinds of column occur.
        p.soil_horizon.as_mut().unwrap().max_slope = 0;
        let h = p
            .soil_horizon
            .clone()
            .expect("the shipped params lay a horizon");
        let m = t.materials();
        let (loam, topsoil) = (m.id(&h.material).unwrap(), m.id("topsoil").unwrap());
        assert!(m.has_tag(loam, "porous"), "the horizon holds rain");
        let cap = m.get(loam).unwrap().field_capacity;
        let init = SoilInit::new(SEED, &p, &t).unwrap();
        let g = Generator::new(SEED, &p, m)
            .unwrap()
            .with_soil(SoilInit::new(SEED, &p, &t).unwrap());
        let plain = Columns::new(&Generator::new(SEED, &p, m).unwrap());
        let cols = Columns::new(&g);
        let (mut with, mut steep, mut masked) = (0, 0, 0);
        for z in 1..63 {
            for x in 1..63 {
                let top = g.surface_height(x, z);
                let n = init.horizon_cells(x, z, |x, z| g.surface_height(x, z));
                let gentle = SIDES
                    .iter()
                    .all(|&(dx, dz)| (g.surface_height(x + dx, z + dz) - top).abs() <= h.max_slope);
                if n == 0 {
                    if gentle {
                        masked += 1;
                    } else {
                        steep += 1;
                    }
                } else {
                    assert!(gentle, "({x}, {z}) is steep");
                    assert!((h.min_cells..=h.max_cells).contains(&n));
                    with += 1;
                }
                for depth in 0..8 {
                    let c = CellPos::new(x, top - depth, z);
                    let (was, ..) = plain.cell(c);
                    let (now, chunk, l) = cols.cell(c);
                    if depth < n && init.is_soil(was) {
                        assert_eq!(now, loam, "({x}, {z}) depth {depth}");
                        assert_eq!(
                            chunk.layers.get_u8(LayerId::Moisture, l),
                            h.moisture.min(cap),
                            "damp from the start"
                        );
                    } else {
                        assert_eq!(now, was, "({x}, {z}) depth {depth} untouched");
                    }
                }
                if n == 0 {
                    assert_eq!(cols.cell(CellPos::new(x, top, z)).0, topsoil);
                }
            }
        }
        assert!(
            with > 0 && steep > 0 && masked > 0,
            "{with} {steep} {masked}"
        );
    }

    /// `(substance index, lo, hi)` of one soil material.
    type Ranges = Vec<(u8, i32, i32)>;

    #[test]
    fn surface_soil_holds_nutrients_in_range() {
        let t = tables();
        let p = params();
        let f = p
            .fertility
            .clone()
            .expect("the shipped params seed nutrients");
        let m = t.materials();
        let init = SoilInit::new(SEED, &p, &t).unwrap();
        let g = Generator::new(SEED, &p, m)
            .unwrap()
            .with_soil(SoilInit::new(SEED, &p, &t).unwrap());
        let cols = Columns::new(&g);
        let ranges: Vec<(MaterialId, Ranges)> = f
            .soils
            .iter()
            .map(|s| {
                let r = s
                    .substances
                    .iter()
                    .map(|(k, lo, hi)| {
                        (
                            t.substance_index(k).unwrap(),
                            i32::from(*lo),
                            i32::from(*hi),
                        )
                    })
                    .collect();
                (m.id(&s.material).unwrap(), r)
            })
            .collect();
        let mut seen = vec![0; ranges.len()];
        // Top-cell nitrogen of the lowest and highest columns.
        let n = t.substance_index("n").unwrap();
        let mut by_height: Vec<(i32, i32)> = Vec::new();
        for z in 1..63 {
            for x in 1..63 {
                let top = g.surface_height(x, z);
                let fert = init.fertility(x, z, top);
                assert!((0..=FULL).contains(&fert));
                for depth in 0..8 {
                    let (now, chunk, l) = cols.cell(CellPos::new(x, top - depth, z));
                    let Some(k) = ranges.iter().position(|(mm, _)| *mm == now) else {
                        continue;
                    };
                    seen[k] += 1;
                    for &(x, lo, hi) in &ranges[k].1 {
                        let v = i32::from(chunk.layers.get_u8(LayerId::Substance(x), l));
                        assert!((lo..=hi).contains(&v), "{v} not in {lo}..={hi}");
                        assert_eq!(v, lo + (hi - lo) * fert / FULL);
                    }
                }
                let (top_m, chunk, l) = cols.cell(CellPos::new(x, top, z));
                if top_m == ranges[0].0 {
                    let v = i32::from(chunk.layers.get_u8(LayerId::Substance(n), l));
                    by_height.push((top, v));
                }
            }
        }
        assert!(seen.iter().all(|&s| s > 0), "every soil seen: {seen:?}");
        by_height.sort_unstable();
        let q = by_height.len() / 4;
        let mean = |s: &[(i32, i32)]| s.iter().map(|&(_, v)| v).sum::<i32>() / s.len() as i32;
        let (valleys, ridges) = (
            mean(&by_height[..q]),
            mean(&by_height[by_height.len() - q..]),
        );
        assert!(valleys > ridges, "valleys {valleys} vs ridges {ridges}");
    }

    #[test]
    fn unknown_names_are_refused() {
        let t = tables();
        let mut p = params();
        p.soil_horizon.as_mut().unwrap().material = "stone".into();
        assert!(matches!(
            SoilInit::new(SEED, &p, &t),
            Err(GenError::MissingTag { .. })
        ));
        let mut p = params();
        p.fertility.as_mut().unwrap().soils[0].substances[0].0 = "nope".into();
        assert!(matches!(
            SoilInit::new(SEED, &p, &t),
            Err(GenError::Invalid { .. })
        ));
        let mut p = params();
        p.fertility.as_mut().unwrap().soils[0].substances[0].1 = 200;
        assert!(SoilInit::new(SEED, &p, &t).is_err(), "lo above hi");
    }
}
