//! Step 2 of chunk generation (SAD §9): the strata under the soil, by depth
//! under each column's surface cell, every boundary moved by its own 2D
//! noise.
//!
//! SAD-P2 §8 adds a limestone band between stone and granite. It is a
//! stratum like the others, named in `worldgen.ron`; its boundaries lie far
//! enough apart (more than twice `boundary_amplitude`) that it never pinches
//! out.

use rmc_world::materials::{MaterialId, MaterialTable};

use crate::r#gen::{STONE_TAG, resolve};
use crate::noise::{LAYER_STRATA, NoiseLayer};
use crate::params::{GenError, StrataParams};

struct Stratum {
    material: MaterialId,
    /// Boundary under this stratum (before noise) and its noise; `None` for
    /// the last stratum.
    until: Option<(i32, NoiseLayer)>,
}

/// The resolved strata for one seed, top down.
pub(crate) struct Strata {
    layers: Vec<Stratum>,
    boundary_amplitude: i32,
}

impl Strata {
    /// Resolves every stratum material (it needs the `stone` tag) and seeds
    /// the boundary under stratum `i` from `(seed, LAYER_STRATA + i)`.
    pub(crate) fn new(
        seed: u64,
        params: &StrataParams,
        materials: &MaterialTable,
    ) -> Result<Self, GenError> {
        let layers = params
            .layers
            .iter()
            .enumerate()
            .map(|(i, l)| {
                Ok(Stratum {
                    material: resolve(
                        materials,
                        format!("strata.layers[{i}]"),
                        &l.material,
                        STONE_TAG,
                    )?,
                    until: l.until_depth.map(|d| {
                        (
                            d,
                            NoiseLayer::new(seed, LAYER_STRATA + i as u32, &params.boundary_noise),
                        )
                    }),
                })
            })
            .collect::<Result<_, GenError>>()?;
        Ok(Self {
            layers,
            boundary_amplitude: params.boundary_amplitude,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.layers.len()
    }

    /// Replaces `ends` with the depth (0 = surface cell) where each bounded
    /// stratum of column `(x, z)` ends, top down.
    pub(crate) fn column_ends(&self, x: i32, z: i32, ends: &mut Vec<i32>) {
        ends.clear();
        ends.extend(self.layers.iter().filter_map(|s| {
            s.until
                .as_ref()
                .map(|(depth, noise)| depth + noise.offset_2d(x, z, self.boundary_amplitude))
        }));
    }

    /// The stratum at `depth` under the surface cell of a column whose
    /// [`column_ends`](Self::column_ends) are `ends`: the first one that
    /// ends below `depth`, else the last.
    pub(crate) fn material(&self, ends: &[i32], depth: i64) -> MaterialId {
        let i = ends
            .iter()
            .position(|&e| depth < i64::from(e))
            .unwrap_or(self.layers.len() - 1);
        self.layers[i].material
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use rmc_eco::EcoTables;
    use rmc_world::coords::{CHUNK_SIZE, CHUNK_VOLUME, ChunkPos, LocalPos};
    use rmc_world::layers::LayerId;
    use rmc_world::materials::{MaterialId, MaterialTable, Phase};

    use crate::r#gen::STONE_TAG;
    use crate::params::WorldBounds;
    use crate::{GenParams, Generator, WaterTableInit};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    const SEED: u64 = 23;

    #[test]
    fn limestone_band_present_and_porous() {
        let table = Arc::new(MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap());
        let mut p = GenParams::load(format!("{ASSETS}/worldgen.ron")).unwrap();
        let key = |m: MaterialId| table.get(m).unwrap().key.as_str();
        let id = |k: &str| table.id(k).unwrap_or_else(|| panic!("no material {k:?}"));

        // SAD-P2 §5: limestone is a porous carbonate stone that holds water
        // and weathers to lime.
        let limestone = id("limestone");
        let d = table.get(limestone).unwrap();
        assert_eq!(d.phase, Phase::Solid);
        assert!(d.diggable);
        for tag in [STONE_TAG, "carbonate", "porous"] {
            assert!(d.has_tag(tag), "limestone lacks {tag}");
        }
        assert_eq!((d.permeability, d.field_capacity), (32, 24));
        assert_eq!(d.weathering_rate, 2);
        assert_eq!(d.composition, [("lime".to_string(), 120)]);

        // SAD-P2 §8: the band is the stratum between stone and granite, and
        // its boundaries lie more than twice the noise amplitude apart, so
        // it never pinches out.
        let layers = &p.strata.layers;
        let at = layers
            .iter()
            .position(|l| l.material == "limestone")
            .expect("a limestone stratum");
        assert!(at > 0 && at + 1 < layers.len());
        assert_eq!(layers[at - 1].material, "stone");
        assert_eq!(layers[at + 1].material, "granite");
        let (top, bottom) = (
            layers[at - 1].until_depth.unwrap(),
            layers[at].until_depth.unwrap(),
        );
        let amp = p.strata.boundary_amplitude;
        assert!(amp > 0, "the band is noise-perturbed");
        assert!(
            bottom - top > 2 * amp,
            "{top}..{bottom} ± {amp} can pinch out"
        );

        // On a 128 × 128 × 128 world, every column has the band right under
        // stone and right over granite, starting and ending within the
        // noise of its boundaries; both move from column to column.
        p.bounds = WorldBounds {
            min: [0, 0, 0],
            max: [127, 127, 127],
        };
        let tables = EcoTables::load_with_materials(Arc::clone(&table), ASSETS).unwrap();
        let g = Generator::new(SEED, &p, &table)
            .unwrap()
            .with_water_table(WaterTableInit::new(&tables));
        let (stone, granite) = (id("stone"), id("granite"));
        let (mut starts, mut thicknesses) = (BTreeSet::new(), BTreeSet::new());
        let chunks_y = 128 / CHUNK_SIZE;
        for cz in 0..4 {
            for cx in 0..4 {
                let vols: Vec<_> = (0..chunks_y)
                    .map(|cy| g.base_volume(ChunkPos::new(cx, cy, cz)))
                    .collect();
                for z in 0..CHUNK_SIZE {
                    for x in 0..CHUNK_SIZE {
                        let (wx, wz) = (cx * CHUNK_SIZE + x, cz * CHUNK_SIZE + z);
                        let surface = g.surface_height(wx, wz);
                        let at = |y: i32| vols[(y / CHUNK_SIZE) as usize].get(x, y % CHUNK_SIZE, z);
                        let band: Vec<i32> =
                            (0..=surface).filter(|&y| at(y) == limestone).collect();
                        let (lo, hi) = (*band.first().unwrap(), *band.last().unwrap());
                        let col = format!("({wx}, {wz})");
                        assert_eq!(band.len() as i32, hi - lo + 1, "{col}: one band");
                        assert_eq!(
                            at(hi + 1),
                            stone,
                            "{col}: stone above, got {}",
                            key(at(hi + 1))
                        );
                        assert_eq!(
                            at(lo - 1),
                            granite,
                            "{col}: granite below, got {}",
                            key(at(lo - 1))
                        );
                        let start = surface - hi;
                        let end = surface - lo + 1;
                        assert!(
                            (top - amp..=top + amp).contains(&start),
                            "{col}: starts {start}"
                        );
                        assert!(
                            (bottom - amp..=bottom + amp).contains(&end),
                            "{col}: ends {end}"
                        );
                        starts.insert(start);
                        thicknesses.insert(end - start);
                    }
                }
            }
        }
        assert!(starts.len() >= 3, "band top barely moves: {starts:?}");
        assert!(
            thicknesses.len() >= 3,
            "band thickness barely moves: {thicknesses:?}"
        );

        // In the generated world (caves, ores, shell, water table), limestone
        // below the water table holds its field_capacity and above it is dry.
        let (mut wet, mut dry) = (0, 0);
        for (pos, chunk) in g.generate_world() {
            let origin = pos.origin_cell().0;
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                if chunk.storage.get(l) != limestone {
                    continue;
                }
                let c = origin + l.0.as_ivec3();
                let (x, y, z) = (c.x, c.y, c.z);
                let moisture = chunk.layers.get_u8(LayerId::Moisture, l);
                if y < g.water_table_y(x, z).unwrap() {
                    assert_eq!(moisture, d.field_capacity, "({x}, {y}, {z})");
                    wet += 1;
                } else {
                    assert_eq!(moisture, 0, "({x}, {y}, {z})");
                    dry += 1;
                }
            }
        }
        assert!(
            wet > 1000 && dry > 1000,
            "limestone cells: {wet} wet, {dry} dry"
        );
    }
}
