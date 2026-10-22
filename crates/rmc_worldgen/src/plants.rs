//! Surface plants (SAD-P2 §8). After the light layer is written, each
//! column's topmost non-air cell is a candidate anchor and the air cell
//! above it a candidate plant cell. The species with a `worldgen_chance`
//! (moss and grass in the shipped `plants.ron`) are tried in row order: the
//! first whose seeded cell hash rolls below its chance, whose `anchor_tags`
//! the anchor carries and whose needs hold at season 0 starts there, as a
//! fresh plant (growth 0, a default [`PlantState`]).
//!
//! Plant materials are passable and let light through, so planting leaves
//! the light layer as the light pass wrote it.

use std::collections::BTreeMap;

use rmc_eco::{EcoTables, PlantStarts};
use rmc_world::chunk::Chunk;
use rmc_world::coords::{CellAabb, CellPos, ChunkPos, LocalPos};
use rmc_world::layers::{LayerId, PlantState};
use rmc_world::materials::MaterialId;

use crate::noise::{LAYER_PLANTS, cell_hash};

/// The low byte of a hash: a roll in 0..=255.
const ROLL_MASK: u64 = 0xFF;

/// What worldgen needs from the eco tables to seed surface plants.
#[derive(Clone, Debug)]
pub struct PlantInit {
    starts: PlantStarts,
    /// Role `air`.
    air: MaterialId,
}

impl PlantInit {
    /// From the eco tables, whose material table must be the one worldgen
    /// generates with.
    pub fn new(tables: &EcoTables) -> Self {
        Self {
            starts: PlantStarts::new(tables),
            air: tables.role_material("air").unwrap_or(MaterialId::AIR),
        }
    }

    /// The seeded roll of species `species` at `cell`, 0..=255.
    pub fn roll(seed: u64, species: u8, cell: CellPos) -> u8 {
        let c = cell.0;
        let h = cell_hash(seed, LAYER_PLANTS + u32::from(species), c.x, c.y, c.z);
        (h & ROLL_MASK) as u8
    }

    /// Seeds the surface plants of every column of `bounds` into `chunks`,
    /// reading the layers already written (temperature, water table,
    /// light). Cells of `bounds` in no given chunk count as air with every
    /// layer 0, so `chunks` should cover the bounds.
    pub fn fill(&self, seed: u64, bounds: CellAabb, chunks: &mut [(ChunkPos, Chunk)]) {
        let starts = self.starts(seed, bounds, chunks);
        let index = chunk_index(chunks);
        for &(cell, species, material) in &starts {
            let (pos, local) = cell.split();
            let chunk = &mut chunks[index[&pos.0.to_array()]].1;
            chunk.storage.set(local, material);
            chunk.plants.insert(
                local.index(),
                PlantState {
                    species,
                    ..PlantState::default()
                },
            );
        }
        for (_, chunk) in chunks.iter_mut() {
            if !chunk.plants.is_empty() {
                chunk.storage.compact();
            }
        }
        tracing::info!(plants = starts.len(), "surface plants seeded");
    }

    /// `(plant cell, species, material)` of every plant to start, in
    /// column (z, x) order.
    fn starts(
        &self,
        seed: u64,
        bounds: CellAabb,
        chunks: &[(ChunkPos, Chunk)],
    ) -> Vec<(CellPos, u8, MaterialId)> {
        let index = chunk_index(chunks);
        let at = |c: CellPos| -> Option<(&Chunk, LocalPos)> {
            if !bounds.contains(c) {
                return None;
            }
            let (pos, local) = c.split();
            index.get(&pos.0.to_array()).map(|&i| (&chunks[i].1, local))
        };
        let material = |c: CellPos| at(c).map_or(self.air, |(ch, l)| ch.storage.get(l));
        let layer = |c: CellPos, id: LayerId| {
            at(c).map_or(0, |(ch, l)| {
                if id.is_i16() {
                    i32::from(ch.layers.get_i16(id, l))
                } else {
                    i32::from(ch.layers.get_u8(id, l))
                }
            })
        };
        let (lo, hi) = (bounds.min.0, bounds.max.0);
        let mut out = Vec::new();
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                // The topmost non-air cell, with an air cell above it.
                let Some(top) = (lo.y..=hi.y)
                    .rev()
                    .find(|&y| material(CellPos::new(x, y, z)) != self.air)
                    .filter(|&y| y < hi.y)
                else {
                    continue;
                };
                let anchor = material(CellPos::new(x, top, z));
                let cell = CellPos::new(x, top + 1, z);
                let pick = self.starts.seeded().find(|&(species, _, chance)| {
                    Self::roll(seed, species, cell) < chance
                        && self.starts.passes(species, cell, anchor, layer)
                });
                if let Some((species, m, _)) = pick {
                    out.push((cell, species, m));
                }
            }
        }
        out
    }
}

/// Chunk position → index in `chunks`.
fn chunk_index(chunks: &[(ChunkPos, Chunk)]) -> BTreeMap<[i32; 3], usize> {
    chunks
        .iter()
        .enumerate()
        .map(|(i, (pos, _))| (pos.0.to_array(), i))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use rmc_eco::LightPass;
    use rmc_eco::tables::{EcoSources, PlantDef};
    use rmc_world::materials::{MaterialDef, MaterialTable};
    use rmc_world::world::World;

    use crate::params::{NoiseParams, WaterTableParams, WorldBounds};
    use crate::{GenParams, Generator, LightInit, MagmaInit, TemperatureInit, WaterTableInit};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    const SEED: u64 = 11;

    /// The shipped materials plus a porous soil that holds water, so the
    /// water table can wet the surface.
    fn materials() -> Arc<MaterialTable> {
        let base = MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap();
        let mut defs: Vec<MaterialDef> = base.iter().cloned().collect();
        let mut d: MaterialDef = ron::from_str(
            r#"(id: 0, key: "test_loam", phase: Solid, tags: ["soil", "porous"], hardness: 10,
                diggable: true, yield_count: 1, color: (120, 90, 60, 255), emissive: 0.0,
                permeability: 64, field_capacity: 40, anchor: true)"#,
        )
        .unwrap();
        d.id = MaterialId(defs.len() as u16);
        defs.push(d);
        Arc::new(MaterialTable::from_defs(defs).unwrap())
    }

    /// The shipped eco tables with `edit` applied to `plants.ron`.
    fn tables(m: &Arc<MaterialTable>, edit: impl Fn(&mut String)) -> EcoTables {
        let mut src = EcoSources::read(ASSETS).unwrap();
        edit(&mut src.plants);
        EcoTables::from_sources(Arc::clone(m), &src).unwrap()
    }

    fn replace_once(s: &mut String, from: &str, to: &str) {
        assert_eq!(s.matches(from).count(), 1, "{from:?} once in plants.ron");
        *s = s.replace(from, to);
    }

    /// The shipped params on a 64 × 128 × 64 world whose top soil is the
    /// porous loam, under a water table that cuts through the surface
    /// (86..=106), so some columns are wet at the top and some dry.
    fn params() -> GenParams {
        let mut p = GenParams::load(format!("{ASSETS}/worldgen.ron")).unwrap();
        p.bounds = WorldBounds {
            min: [0, 0, 0],
            max: [63, 127, 63],
        };
        p.soil[0].material = "test_loam".into();
        p.water_table = Some(WaterTableParams {
            level: 96,
            amplitude: 10,
            noise: NoiseParams {
                frequency: 0.05,
                octaves: 1,
            },
        });
        p
    }

    fn generator(p: &GenParams, m: &MaterialTable, t: &EcoTables) -> Generator {
        Generator::new(SEED, p, m)
            .unwrap()
            .with_temperature(TemperatureInit::new(t))
            .with_water_table(WaterTableInit::new(t))
            .with_magma(MagmaInit::new(t))
            .with_light(LightInit::new(t))
    }

    fn world(m: &Arc<MaterialTable>, t: &EcoTables, chunks: Vec<(ChunkPos, Chunk)>) -> World {
        let bounds = params().bounds.aabb();
        let mut w = World::new(Arc::clone(m), bounds);
        w.set_eco_rules(t.displacement_rules());
        for (pos, chunk) in chunks {
            assert_eq!(chunk.eco_revision, 0, "{pos:?}: worldgen state");
            w.insert_chunk(pos, chunk).unwrap();
        }
        w
    }

    /// Whether `p`'s anchor tags and needs (§7.8 step 1) hold for a plant
    /// at `cell`, read straight from the world's layers.
    fn passes(w: &World, t: &EcoTables, p: &PlantDef, cell: CellPos) -> bool {
        let below = CellPos::new(cell.0.x, cell.0.y - 1, cell.0.z);
        let m = t.materials();
        let anchor = m.get(w.get_cell(below).unwrap()).unwrap();
        let sub = |key: &str| w.layer(below, LayerId::Substance(t.substance_index(key).unwrap()));
        let temp = w.layer(cell, LayerId::Temperature);
        p.anchor_tags.iter().any(|tag| anchor.has_tag(tag))
            && w.layer(cell, LayerId::Light) >= i32::from(p.needs.light)
            && (i32::from(p.needs.temp.0)..=i32::from(p.needs.temp.1)).contains(&temp)
            && w.layer(below, LayerId::Moisture) >= i32::from(p.needs.moisture)
            && p.needs
                .substance_min
                .iter()
                .all(|(k, n)| sub(k) >= i32::from(*n))
            && p.needs
                .substance_max
                .iter()
                .all(|(k, n)| sub(k) <= i32::from(*n))
    }

    #[derive(Debug, Default)]
    struct Stats {
        /// Plants per species index.
        planted: BTreeMap<u8, usize>,
        /// Sites where no species passes (anchor tags or needs).
        none_passes: usize,
        /// Per species: sites where it passes and its roll fails (always,
        /// for a species with no worldgen chance).
        unrolled: BTreeMap<u8, usize>,
        /// Per species: sites where its roll passes but its anchor tags do
        /// not.
        wrong_anchor: BTreeMap<u8, usize>,
    }

    /// Checks every column of `w` against the rule, independently of
    /// `PlantInit`: the site above the topmost non-plant, non-air cell holds
    /// exactly the first species in row order with a worldgen chance whose
    /// roll is below it and that passes there, as a fresh plant; no other
    /// cell holds a plant.
    fn check(w: &World, t: &EcoTables) -> Stats {
        let m = t.materials();
        let air = t.role_material("air").unwrap();
        let bounds = w.bounds();
        let (lo, hi) = (bounds.min.0, bounds.max.0);
        let mut stats = Stats::default();
        let mut sites = 0;
        for z in lo.z..=hi.z {
            for x in lo.x..=hi.x {
                let Some(top) = (lo.y..=hi.y)
                    .rev()
                    .find(|&y| {
                        let c = CellPos::new(x, y, z);
                        w.get_cell(c) != Some(air) && w.plant(c).is_none()
                    })
                    .filter(|&y| y < hi.y)
                else {
                    continue;
                };
                let cell = CellPos::new(x, top + 1, z);
                let anchor = m.get(w.get_cell(CellPos::new(x, top, z)).unwrap()).unwrap();
                let mut expect = None;
                let mut any = false;
                for (i, p) in t.plants().iter().enumerate() {
                    let i = i as u8;
                    // A chance of 0 never rolls.
                    let rolled = PlantInit::roll(SEED, i, cell) < p.worldgen_chance;
                    let pass = passes(w, t, p, cell);
                    any |= pass;
                    if rolled && pass && expect.is_none() {
                        expect = Some(i);
                    }
                    if pass && !rolled {
                        *stats.unrolled.entry(i).or_default() += 1;
                    }
                    if rolled && !p.anchor_tags.iter().any(|tag| anchor.has_tag(tag)) {
                        *stats.wrong_anchor.entry(i).or_default() += 1;
                    }
                }
                stats.none_passes += usize::from(!any);
                let got = w.plant(cell).copied();
                match expect {
                    Some(i) => {
                        let state = PlantState {
                            species: i,
                            ..PlantState::default()
                        };
                        assert_eq!(got, Some(state), "{cell:?}: a fresh plant");
                        let material = m.id(&t.plants()[usize::from(i)].material);
                        assert_eq!(w.get_cell(cell), material, "{cell:?}");
                        assert_eq!(w.layer(cell, LayerId::Growth), 0, "{cell:?}");
                        *stats.planted.entry(i).or_default() += 1;
                    }
                    None => {
                        assert_eq!(got, None, "{cell:?}: no plant passes here");
                        assert_eq!(w.get_cell(cell), Some(air), "{cell:?}");
                    }
                }
                sites += 1;
            }
        }
        assert_eq!(sites, 62 * 62, "every column inside the walls");
        let plants: usize = w.iter_chunks_sorted().map(|(_, c)| c.plants.len()).sum();
        let planted: usize = stats.planted.values().sum();
        assert_eq!(plants, planted, "plants only at the surface sites");
        stats
    }

    #[test]
    fn initial_plants_only_on_passing_anchors() {
        let m = materials();
        let p = params();
        // Grass needs nitrogen, which worldgen never lays down; without
        // that need it can start on the wet loam too.
        let t = tables(&m, |s| replace_once(s, "substance_min: [(\"n\", 2)],", ""));
        let species = |key: &str| t.plants().iter().position(|p| p.key == key).unwrap() as u8;
        let (moss, grass, fireweed) = (species("moss"), species("grass"), species("fireweed"));
        assert!(t.plants()[usize::from(moss)].worldgen_chance > 0);
        assert!(t.plants()[usize::from(grass)].worldgen_chance > 0);
        assert_eq!(t.plants()[usize::from(fireweed)].worldgen_chance, 0);

        let g = generator(&p, &m, &t).with_plants(PlantInit::new(&t));
        let chunks = g.generate_world();
        let hash = |chunks: &[(ChunkPos, Chunk)]| -> Vec<u64> {
            chunks.iter().map(|(_, c)| c.hash()).collect()
        };
        assert_eq!(hash(&g.generate_world()), hash(&chunks), "deterministic");
        let mut w = world(&m, &t, chunks.clone());
        let stats = check(&w, &t);
        let n = |map: &BTreeMap<u8, usize>, s: u8| map.get(&s).copied().unwrap_or(0);
        assert!(n(&stats.planted, moss) > 50, "{stats:?}");
        assert!(n(&stats.planted, grass) > 50, "{stats:?}");
        assert_eq!(n(&stats.planted, fireweed), 0, "{stats:?}");
        // Dry columns pass nothing; wet ones pass but miss their roll.
        assert!(stats.none_passes > 100, "{stats:?}");
        assert!(n(&stats.unrolled, moss) > 50, "{stats:?}");
        // Fireweed passes on the wet loam, but worldgen does not seed it.
        assert!(n(&stats.unrolled, fireweed) > 50, "{stats:?}");

        // Plants let light through: the light pass would write the same.
        let revisions = |w: &World| -> Vec<u64> {
            w.iter_chunks_sorted()
                .map(|(_, c)| c.eco_revision)
                .collect()
        };
        let before = revisions(&w);
        LightPass::new(&t).recompute(&mut w);
        assert_eq!(revisions(&w), before, "planting changes no light");

        // Without plants the world is the same but for the plant cells.
        let bare = generator(&p, &m, &t).generate_world();
        for ((pos, a), (_, b)) in chunks.iter().zip(&bare) {
            assert!(b.plants.is_empty(), "{pos:?}");
            assert_eq!(a.layers, b.layers, "{pos:?}");
            for i in 0..rmc_world::coords::CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                if !a.plants.contains_key(&i) {
                    assert_eq!(a.storage.get(l), b.storage.get(l), "{pos:?} {l:?}");
                }
            }
        }

        // Grass on stone only: the loam fails its anchor tags, so only moss
        // starts, even where grass rolls and its needs hold.
        let stony = tables(&m, |s| {
            replace_once(s, "substance_min: [(\"n\", 2)],", "");
            replace_once(
                s,
                "(key: \"grass\", material: \"grass\", anchor_tags: [\"soil\"]",
                "(key: \"grass\", material: \"grass\", anchor_tags: [\"stone\"]",
            );
        });
        let g = generator(&p, &m, &stony).with_plants(PlantInit::new(&stony));
        let w = world(&m, &stony, g.generate_world());
        let stats = check(&w, &stony);
        assert!(n(&stats.planted, moss) > 50, "{stats:?}");
        assert_eq!(n(&stats.planted, grass), 0, "{stats:?}");
        assert!(n(&stats.wrong_anchor, grass) > 50, "{stats:?}");
    }
}
