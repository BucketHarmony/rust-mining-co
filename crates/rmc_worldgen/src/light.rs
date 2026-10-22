//! The initial light layer (SAD-P2 §8): one full light pass (§7.7) over
//! the generated world, written into every chunk before the world is
//! assembled.

use rmc_eco::{EcoTables, LightPass};
use rmc_world::chunk::Chunk;
use rmc_world::coords::{CellAabb, ChunkPos};
use rmc_world::layers::LayerId;

/// What worldgen needs to write the light layer.
#[derive(Clone, Debug)]
pub struct LightInit {
    pass: LightPass,
}

impl LightInit {
    /// From the eco tables, whose material table must be the one worldgen
    /// generates with.
    pub fn new(tables: &EcoTables) -> Self {
        Self {
            pass: LightPass::new(tables),
        }
    }

    /// Writes the `Light` layer of every chunk in `chunks` from one light
    /// pass over `bounds`. Cells of `bounds` in no given chunk block light,
    /// so `chunks` should cover the bounds.
    pub fn fill(&self, bounds: CellAabb, chunks: &mut [(ChunkPos, Chunk)]) {
        let grid = self
            .pass
            .compute(bounds, chunks.iter().map(|(pos, c)| (*pos, c)));
        for (pos, chunk) in chunks.iter_mut() {
            // Dense (SAD-P2 §3): materialized even where it is all dark.
            chunk.layers.materialize(LayerId::Light);
            chunk
                .layers
                .replace_u8(LayerId::Light, &grid.chunk_values(*pos));
        }
        tracing::info!(chunks = chunks.len(), "initial light written");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use rmc_world::materials::MaterialTable;
    use rmc_world::world::World;

    use crate::params::WorldBounds;
    use crate::{GenParams, Generator, MagmaInit, TemperatureInit, WaterTableInit};

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    const SEED: u64 = 11;

    #[test]
    fn initial_light_nonzero_at_surface() {
        let m = Arc::new(MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap());
        let t = EcoTables::load_with_materials(Arc::clone(&m), ASSETS).unwrap();
        let mut p = GenParams::load(format!("{ASSETS}/worldgen.ron")).unwrap();
        p.bounds = WorldBounds {
            min: [0, 0, 0],
            max: [63, 127, 63],
        };
        let generator = || {
            Generator::new(SEED, &p, &m)
                .unwrap()
                .with_temperature(TemperatureInit::new(&t))
                .with_water_table(WaterTableInit::new(&t))
                .with_magma(MagmaInit::new(&t))
        };
        let lit = generator().with_light(LightInit::new(&t));
        let bounds = lit.bounds();
        let mut w = World::new(Arc::clone(&m), bounds);
        w.set_eco_rules(t.displacement_rules());
        for (pos, chunk) in lit.generate_world() {
            assert!(chunk.layers.contains(LayerId::Light), "{pos:?}");
            assert_eq!(chunk.eco_revision, 0, "{pos:?}: worldgen state");
            w.insert_chunk(pos, chunk).unwrap();
        }

        // Every column: the cell above the surface is in full light, and a
        // surface that lets light in (water, ice) is lit itself.
        let top = bounds.max.0.y;
        let (mut columns, mut lit_surfaces) = (0, 0);
        for z in bounds.min.0.z..=bounds.max.0.z {
            for x in bounds.min.0.x..=bounds.max.0.x {
                let s = i32::from(w.surface_y(x, z));
                if s == top {
                    // The bedrock walls rise to the top of the world.
                    let wall = w
                        .get_cell(rmc_world::coords::CellPos::new(x, s, z))
                        .unwrap();
                    assert!(!m.get(wall).unwrap().diggable, "({x}, {z}): a wall");
                    continue;
                }
                let above = rmc_world::coords::CellPos::new(x, s + 1, z);
                assert_eq!(w.layer(above, LayerId::Light), 255, "{above:?}");
                let surface = rmc_world::coords::CellPos::new(x, s, z);
                let id = w.get_cell(surface).unwrap();
                if m.get(id).unwrap().light_attenuation() < u8::MAX {
                    assert!(w.layer(surface, LayerId::Light) > 0, "{surface:?}");
                    lit_surfaces += 1;
                }
                columns += 1;
            }
        }
        assert_eq!(columns, 62 * 62, "every column inside the walls");
        tracing::debug!(lit_surfaces, "surface cells that let light in");

        // It is the light pass's own result: recomputing changes nothing.
        let revisions = |w: &World| -> Vec<u64> {
            w.iter_chunks_sorted()
                .map(|(_, c)| c.eco_revision)
                .collect()
        };
        let before = revisions(&w);
        LightPass::new(&t).recompute(&mut w);
        assert_eq!(revisions(&w), before, "worldgen light is the pass's");

        // Without `with_light`, chunks carry no light layer, as before.
        for (pos, chunk) in generator().generate_chunks(bounds.chunks().take(4)) {
            assert!(!chunk.layers.contains(LayerId::Light), "{pos:?}");
        }
    }
}
