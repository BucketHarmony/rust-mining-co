//! The initial water table (SAD-P2 §8). Below each column's
//! `water_table_y`, porous solids hold their `field_capacity` of moisture
//! and caves are flooded with water at fill 255. Above it the world is dry.
//!
//! A cave cell is an air cell under the column's surface: worldgen leaves
//! air under the surface only where it carved a cave.

use rmc_eco::EcoTables;
use rmc_world::chunk::Chunk;
use rmc_world::coords::{CHUNK_SIZE, ChunkPos, LocalPos};
use rmc_world::layers::LayerId;
use rmc_world::materials::{MaterialId, Phase};

/// The tag that makes a solid porous (SAD-P2 §5).
const POROUS: &str = "porous";
/// The fill of a flooded cave cell.
const FULL: u8 = u8::MAX;

/// What worldgen needs from the eco tables to lay down the water table.
#[derive(Clone, Debug)]
pub struct WaterTableInit {
    /// Role `water`, if the material table has it.
    water: Option<MaterialId>,
    /// Per material id: `field_capacity` of a porous solid, 0 for every
    /// other material.
    capacity: Vec<u8>,
}

impl WaterTableInit {
    /// From the eco tables, whose material table must be the one worldgen
    /// generates with.
    pub fn new(tables: &EcoTables) -> Self {
        Self {
            water: tables.role_material("water"),
            capacity: tables
                .materials()
                .iter()
                .map(|d| {
                    if d.phase == Phase::Solid && d.has_tag(POROUS) {
                        d.field_capacity
                    } else {
                        0
                    }
                })
                .collect(),
        }
    }

    /// Floods the caves and wets the porous cells of the generated chunk at
    /// `pos` below `table(x, z)`. `surface(x, z)` is the column's topmost
    /// solid cell after step 1.
    pub fn fill(
        &self,
        chunk: &mut Chunk,
        pos: ChunkPos,
        surface: impl Fn(i32, i32) -> i32,
        table: impl Fn(i32, i32) -> i32,
    ) {
        let origin = pos.origin_cell().0;
        let mut flooded = false;
        for z in 0..CHUNK_SIZE {
            for x in 0..CHUNK_SIZE {
                let (cx, cz) = (origin.x + x, origin.z + z);
                let top = table(cx, cz);
                if top <= origin.y {
                    continue;
                }
                let surface = surface(cx, cz);
                for y in 0..(top - origin.y).min(CHUNK_SIZE) {
                    let l = LocalPos::new(x as u32, y as u32, z as u32);
                    let m = chunk.storage.get(l);
                    if m == MaterialId::AIR {
                        if let Some(water) = self.water
                            && origin.y + y < surface
                        {
                            chunk.storage.set(l, water);
                            chunk.layers.set_u8(LayerId::Moisture, l, FULL);
                            flooded = true;
                        }
                        continue;
                    }
                    let cap = self.capacity.get(usize::from(m.0)).copied().unwrap_or(0);
                    if cap > 0 {
                        chunk.layers.set_u8(LayerId::Moisture, l, cap);
                    }
                }
            }
        }
        if flooded {
            chunk.storage.compact();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use rmc_world::coords::{CHUNK_VOLUME, CellPos};
    use rmc_world::materials::{MaterialDef, MaterialTable};

    use crate::params::{NoiseParams, WaterTableParams, WorldBounds};
    use crate::{GenParams, Generator, TemperatureInit};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    const SEED: u64 = 11;

    /// The shipped material rows plus `rows`, with ids in table order.
    fn table_with(rows: &[&str]) -> Arc<MaterialTable> {
        let base = MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap();
        let mut defs: Vec<MaterialDef> = base.iter().cloned().collect();
        for text in rows {
            let mut d: MaterialDef = ron::from_str(text).unwrap();
            d.id = MaterialId(defs.len() as u16);
            defs.push(d);
        }
        Arc::new(MaterialTable::from_defs(defs).unwrap())
    }

    fn tables(m: &Arc<MaterialTable>) -> EcoTables {
        EcoTables::load_with_materials(Arc::clone(m), ASSETS).unwrap()
    }

    /// The shipped params on a 64 × 128 × 64 world with a water table at
    /// `level` ± `amplitude`.
    fn params(level: i32, amplitude: i32) -> GenParams {
        let mut p = GenParams::load(format!("{ASSETS}/worldgen.ron")).unwrap();
        p.bounds = WorldBounds {
            min: [0, 0, 0],
            max: [63, 127, 63],
        };
        p.water_table = Some(WaterTableParams {
            level,
            amplitude,
            noise: NoiseParams {
                frequency: 0.05,
                octaves: 1,
            },
        });
        p
    }

    /// Calls `f(cell, plain material, generated chunk, local)` for every
    /// cell of every chunk `wet` generates, beside the same chunk from
    /// `plain`.
    fn for_each_cell(
        wet: &Generator,
        plain: &Generator,
        mut f: impl FnMut(CellPos, MaterialId, &Chunk, LocalPos),
    ) {
        for pos in wet.bounds().chunks() {
            let (w, p) = (wet.generate(pos), plain.generate(pos));
            assert_eq!(w, wet.generate(pos), "{pos:?} is deterministic");
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                f(CellPos::from_chunk_local(pos, l), p.storage.get(l), &w, l);
            }
        }
    }

    #[test]
    fn porous_below_table_at_capacity() {
        let m = table_with(&[
            r#"(id: 0, key: "test_loam", phase: Solid, tags: ["soil", "porous"], hardness: 10,
                diggable: true, yield_count: 1, color: (120, 90, 60, 255),
                emissive: 0.0, permeability: 64, field_capacity: 40)"#,
            r#"(id: 0, key: "test_aquifer", phase: Solid, tags: ["stone", "porous"],
                hardness: 30, diggable: true, yield_count: 1, color: (200, 200, 180, 255),
                emissive: 0.0, permeability: 32, field_capacity: 24)"#,
            r#"(id: 0, key: "test_sieve", phase: Solid, tags: ["stone", "porous"],
                hardness: 30, diggable: true, yield_count: 1, color: (200, 180, 180, 255),
                emissive: 0.0, permeability: 32, field_capacity: 0)"#,
        ]);
        let t = tables(&m);
        let (loam, aquifer, sieve) = (
            m.id("test_loam").unwrap(),
            m.id("test_aquifer").unwrap(),
            m.id("test_sieve").unwrap(),
        );
        // Soil: topsoil, then loam. Strata: stone, the aquifer (depth
        // 24..56 under a surface at 86..=106), then a porous rock that holds
        // nothing. The table, at 64 ± 6, runs through the aquifer.
        let mut p = params(64, 6);
        p.soil[1].material = "test_loam".into();
        p.strata.layers[1].material = "test_aquifer".into();
        p.strata.layers[2].material = "test_sieve".into();
        // No caves: this test is about porous rock.
        p.caves.blob.threshold = 2.0;
        p.caves.worm.radius = 0.0;
        let wet = Generator::new(SEED, &p, &m)
            .unwrap()
            .with_water_table(WaterTableInit::new(&t));
        let plain = Generator::new(SEED, &p, &m).unwrap();

        // The table is a per-column function: level ± amplitude, following
        // its noise.
        let (mut lo, mut hi) = (i32::MAX, i32::MIN);
        for z in 0..64 {
            for x in 0..64 {
                let y = wet.water_table_y(x, z).unwrap();
                assert!((58..=70).contains(&y), "({x}, {z}): table at {y}");
                (lo, hi) = (lo.min(y), hi.max(y));
            }
        }
        assert!(lo < hi, "the table follows its noise: {lo}..={hi}");
        assert_eq!(plain.water_table_y(0, 0), wet.water_table_y(0, 0));

        let mut counts = BTreeMap::new();
        for_each_cell(&wet, &plain, |cell, was, chunk, l| {
            let (x, y, z) = (cell.0.x, cell.0.y, cell.0.z);
            let below = y < wet.water_table_y(x, z).unwrap();
            let moisture = chunk.layers.get_u8(LayerId::Moisture, l);
            assert_eq!(
                chunk.storage.get(l),
                was,
                "{cell:?}: no cave, no cell changes"
            );
            let want = if below && was == aquifer {
                24
            } else if below && was == loam {
                40
            } else {
                0
            };
            assert_eq!(moisture, want, "{cell:?}: {was:?}, below {below}");
            *counts.entry((was, below)).or_insert(0) += 1;
        });
        let n = |m: MaterialId, below: bool| counts.get(&(m, below)).copied().unwrap_or(0);
        assert!(n(aquifer, true) > 1000, "{counts:?}");
        assert!(n(aquifer, false) > 1000, "{counts:?}");
        assert!(n(loam, false) > 1000, "{counts:?}");
        assert!(n(sieve, true) > 1000, "{counts:?}");
        assert_eq!(n(loam, true), 0, "the soil lies above the table");

        // A table among the hills wets the soil too; the porous rock with
        // no capacity and the sky stay dry.
        let mut high = p.clone();
        high.water_table.as_mut().unwrap().level = 100;
        let high = Generator::new(SEED, &high, &m)
            .unwrap()
            .with_water_table(WaterTableInit::new(&t));
        let mut wet_loam = 0;
        for_each_cell(&high, &plain, |cell, was, chunk, l| {
            let below = cell.0.y < high.water_table_y(cell.0.x, cell.0.z).unwrap();
            let moisture = chunk.layers.get_u8(LayerId::Moisture, l);
            if was == loam && below {
                assert_eq!(moisture, 40, "{cell:?}");
                wet_loam += 1;
            }
            if was == sieve || was == MaterialId::AIR {
                assert_eq!(moisture, 0, "{cell:?}");
            }
        });
        assert!(wet_loam > 1000, "{wet_loam} wet loam cells");

        // No water table in the params, or no eco tables: a dry world, cell
        // for cell the plain one, with no moisture layer.
        let mut none = p.clone();
        none.water_table = None;
        let dry = Generator::new(SEED, &none, &m)
            .unwrap()
            .with_water_table(WaterTableInit::new(&t));
        assert_eq!(dry.water_table_y(5, 5), None);
        for pos in plain.bounds().chunks() {
            let chunk = plain.generate(pos);
            assert!(!chunk.layers.contains(LayerId::Moisture), "{pos:?}");
            assert_eq!(dry.generate(pos), chunk, "{pos:?}");
        }
    }

    #[test]
    fn caves_below_table_flooded() {
        // The game's params lay down a water table, and a lower world
        // keeps its share of the height under it.
        let shipped = GenParams::load(format!("{ASSETS}/worldgen.ron")).unwrap();
        let level = shipped.water_table.as_ref().expect("a water table").level;
        let low = shipped
            .fit_to(WorldBounds {
                min: [0, 0, 0],
                max: [127, 63, 127],
            })
            .unwrap();
        assert_eq!(low.water_table.unwrap().level, level / 2);

        let m = table_with(&[]);
        let t = tables(&m);
        let water = t
            .role_material("water")
            .expect("the shipped table has water");
        let init = TemperatureInit::new(&t);
        let p = params(64, 6);
        let wet = Generator::new(SEED, &p, &m)
            .unwrap()
            .with_water_table(WaterTableInit::new(&t))
            .with_temperature(TemperatureInit::new(&t));
        let plain = Generator::new(SEED, &p, &m).unwrap();
        let bottom = wet.bounds().min.0.y;

        let (mut flooded, mut dry_cave) = (0, 0);
        for_each_cell(&wet, &plain, |cell, was, chunk, l| {
            let (x, y, z) = (cell.0.x, cell.0.y, cell.0.z);
            let below = y < wet.water_table_y(x, z).unwrap();
            let cave = was == MaterialId::AIR && y < wet.surface_height(x, z);
            let got = chunk.storage.get(l);
            let fill = chunk.layers.get_u8(LayerId::Moisture, l);
            if cave && below {
                assert_eq!(got, water, "{cell:?}: a cave below the table floods");
                assert_eq!(fill, FULL, "{cell:?}: at fill 255");
                assert_eq!(
                    chunk.layers.get_i16(LayerId::Temperature, l),
                    init.profile(bottom, y),
                    "{cell:?}: the water starts on the profile"
                );
                flooded += 1;
            } else {
                assert_eq!(got, was, "{cell:?}: only caves below the table change");
                assert_eq!(fill, 0, "{cell:?}: nothing shipped is porous");
                dry_cave += usize::from(cave);
            }
        });
        assert!(flooded > 1000, "{flooded} flooded cave cells");
        assert!(dry_cave > 1000, "{dry_cave} cave cells above the table");

        // A table near the surface floods the caves below it, never the
        // sky, even where it rises above the ground.
        let high = Generator::new(SEED, &params(100, 6), &m)
            .unwrap()
            .with_water_table(WaterTableInit::new(&t));
        let mut sky_below = 0;
        for_each_cell(&high, &plain, |cell, was, chunk, l| {
            let (x, y, z) = (cell.0.x, cell.0.y, cell.0.z);
            let surface = high.surface_height(x, z);
            let below = y < high.water_table_y(x, z).unwrap();
            let got = chunk.storage.get(l);
            if was != MaterialId::AIR {
                return;
            }
            if y >= surface {
                assert_eq!(got, MaterialId::AIR, "{cell:?}: sky");
                assert_eq!(chunk.layers.get_u8(LayerId::Moisture, l), 0, "{cell:?}");
                sky_below += usize::from(below);
            } else if below {
                assert_eq!(got, water, "{cell:?}: a cave below the table floods");
            } else {
                assert_eq!(got, MaterialId::AIR, "{cell:?}: above the table");
            }
        });
        assert!(sky_below > 0, "the high table reaches above the ground");

        // The shipped table is level, so the flooded caves start at rest:
        // no water cell has air below it or beside it, and the water sweep
        // has nothing to move.
        let mut rest = shipped.clone();
        rest.bounds = p.bounds;
        let rest = Generator::new(SEED, &rest, &m)
            .unwrap()
            .with_water_table(WaterTableInit::new(&t));
        let chunks: BTreeMap<_, _> = rest
            .generate_world()
            .into_iter()
            .map(|(pos, c)| (pos.0.to_array(), c))
            .collect();
        let at = |c: CellPos| {
            let (pos, l) = c.split();
            chunks.get(&pos.0.to_array()).map(|ch| ch.storage.get(l))
        };
        let mut resting = 0;
        for (pos, chunk) in &chunks {
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                if chunk.storage.get(l) != water {
                    continue;
                }
                let pos = ChunkPos::new(pos[0], pos[1], pos[2]);
                let c = CellPos::from_chunk_local(pos, l);
                for d in [[0, -1, 0], [0, 0, -1], [-1, 0, 0], [1, 0, 0], [0, 0, 1]] {
                    let n = CellPos::new(c.0.x + d[0], c.0.y + d[1], c.0.z + d[2]);
                    assert_ne!(at(n), Some(MaterialId::AIR), "{c:?} spills into {n:?}");
                }
                resting += 1;
            }
        }
        assert!(resting > 1000, "{resting} water cells at rest");

        // Without a water role the caves stay air.
        let mut rows: Vec<MaterialDef> = m.iter().cloned().collect();
        rows[usize::from(water.0)].key = "not_water".into();
        let m2 = Arc::new(MaterialTable::from_defs(rows).unwrap());
        let t2 = tables(&m2);
        assert_eq!(t2.role_material("water"), None);
        let roleless = Generator::new(SEED, &p, &m2)
            .unwrap()
            .with_water_table(WaterTableInit::new(&t2));
        for pos in plain.bounds().chunks() {
            assert_eq!(roleless.generate(pos), plain.generate(pos), "{pos:?}");
        }
    }
}
