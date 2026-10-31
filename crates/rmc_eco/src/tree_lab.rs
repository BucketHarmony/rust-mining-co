//! The tree lab (docs/TREES.md T4): one graph tree grown headless, for
//! iterating on its look without the game.
//!
//! The lab plants a species on a flat field of its first anchor material
//! (`anchor_tags`) and runs, per flush, the light pass and the extension
//! sweep of the flush's season, with the root's biomass topped up first:
//! a well-fed tree, as in the `trees` tests. Its world is a scratch world
//! that never reaches a game. `tree_lab.ron` gives species their `Graph`
//! form for the lab until the shipped rows switch to it (TREES T7).

use std::path::Path;
use std::sync::Arc;

use rmc_world::chunk::Chunk;
use rmc_world::coords::{CellAabb, CellPos, ChunkPos};
use rmc_world::layers::{LayerId, PlantState};
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::MaterialId;
use rmc_world::tree::TreeGraph;
use rmc_world::world::{World, WorldError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::light::LightPass;
use crate::parts::ExtendPass;
use crate::tables::{EcoSources, EcoTableError, EcoTables, FormDef, PlantDef};

/// The lab's form overrides, in `assets/`.
pub const TREE_LAB_FILE: &str = "tree_lab.ron";

/// The lab world in chunks along x and z; the root stands at its center.
const LAB_CHUNKS: i32 = 3;
/// The lab world in chunks along y; the field's top is the last cell of
/// the lowest.
const LAB_LAYERS: i32 = 3;

/// One row of `tree_lab.ron`: the form a species grows in the lab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabForm {
    pub species: String,
    pub form: FormDef,
}

#[derive(Debug, Error)]
pub enum LabError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {file}: {source}")]
    Parse {
        file: &'static str,
        #[source]
        source: ron::error::SpannedError,
    },
    #[error("writing plants: {0}")]
    Write(#[from] ron::Error),
    #[error(transparent)]
    Tables(#[from] EcoTableError),
    #[error(transparent)]
    World(#[from] WorldError),
    #[error("no species {0:?}")]
    NoSpecies(String),
    #[error("species {0:?} does not grow as a graph")]
    NotGraph(String),
    #[error("no material carries an anchor tag of {0:?}")]
    NoAnchor(String),
}

/// Reads `tree_lab.ron` in `dir`.
pub fn read_forms(dir: impl AsRef<Path>) -> Result<Vec<LabForm>, LabError> {
    let path = dir.as_ref().join(TREE_LAB_FILE);
    let text = std::fs::read_to_string(&path).map_err(|source| LabError::Io {
        path: path.display().to_string(),
        source,
    })?;
    ron::from_str(&text).map_err(|source| LabError::Parse {
        file: TREE_LAB_FILE,
        source,
    })
}

/// `src` with each species of `forms` growing its lab form. A form for a
/// species without parts, or for none, is an error.
pub fn with_forms(src: &EcoSources, forms: &[LabForm]) -> Result<EcoSources, LabError> {
    let mut plants: Vec<PlantDef> =
        ron::from_str(&src.plants).map_err(|source| LabError::Parse {
            file: crate::tables::PLANTS_FILE,
            source,
        })?;
    for f in forms {
        let parts = plants
            .iter_mut()
            .find(|p| p.key == f.species)
            .ok_or_else(|| LabError::NoSpecies(f.species.clone()))?
            .parts
            .as_mut()
            .ok_or_else(|| LabError::NotGraph(f.species.clone()))?;
        parts.form = f.form.clone();
    }
    Ok(EcoSources {
        plants: ron::to_string(&plants)?,
        ..src.clone()
    })
}

/// The tree after one flush.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabFrame {
    pub season: u32,
    /// The season's `seasons.ron` key.
    pub season_key: String,
    /// The flush within the season, from 0.
    pub flush: u32,
    pub graph: TreeGraph,
}

/// Grows `species` from a ripe root for `seasons` seasons of `flushes`
/// flushes each, in a world of seed `seed`, and returns the tree after
/// every flush.
pub fn grow(
    tables: &EcoTables,
    species: &str,
    seed: u64,
    seasons: u32,
    flushes: u32,
) -> Result<Vec<LabFrame>, LabError> {
    let index = tables
        .plants()
        .iter()
        .position(|p| p.key == species)
        .ok_or_else(|| LabError::NoSpecies(species.to_string()))?;
    let row = &tables.plants()[index];
    let parts = match &row.parts {
        Some(parts) if matches!(parts.form, FormDef::Graph { .. }) => parts,
        _ => return Err(LabError::NotGraph(species.to_string())),
    };
    let materials = tables.materials();
    let anchor = materials
        .iter()
        .find(|d| d.tags.iter().any(|t| row.anchor_tags.contains(t)))
        .and_then(|d| materials.id(&d.key))
        .ok_or_else(|| LabError::NoAnchor(species.to_string()))?;
    let wood = materials
        .id(&parts.wood)
        .ok_or_else(|| LabError::NotGraph(species.to_string()))?;
    let air = tables.role_material("air").unwrap_or(MaterialId::AIR);

    let side = LAB_CHUNKS * CHUNK;
    let top = LAB_LAYERS * CHUNK - 1;
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(side - 1, top, side - 1));
    let mut world = World::new(Arc::clone(materials), bounds);
    world.set_eco_rules(tables.displacement_rules());
    let ground = Chunk::uniform(anchor);
    for cz in 0..LAB_CHUNKS {
        for cx in 0..LAB_CHUNKS {
            for cy in 0..LAB_LAYERS {
                let chunk = if cy == 0 {
                    ground.clone()
                } else {
                    Chunk::uniform(air)
                };
                world.insert_chunk(ChunkPos::new(cx, cy, cz), chunk)?;
            }
        }
    }
    let root = CellPos::new(side / 2, CHUNK, side / 2);
    let mut scratch = SeasonLedger::default();
    world.eco_set(root, wood, &mut scratch);
    world.eco_put_plant(
        root,
        Some(PlantState {
            species: u8::try_from(index).unwrap_or(u8::MAX),
            ..PlantState::default()
        }),
    );

    let light = LightPass::new(tables);
    let keys = &tables.seasons().seasons;
    let mut frames = Vec::new();
    for season in 0..seasons {
        let season_key = keys
            .get(season as usize % keys.len().max(1))
            .map(|s| s.key.clone())
            .unwrap_or_default();
        for flush in 0..flushes {
            world.eco_put_layer(root, LayerId::Biomass, i32::from(u8::MAX));
            light.recompute(&mut world);
            ExtendPass::for_season(tables, season, seed).run(&mut world, &mut scratch);
            frames.push(LabFrame {
                season,
                season_key: season_key.clone(),
                flush,
                graph: world.tree(root).cloned().unwrap_or_default(),
            });
        }
    }
    Ok(frames)
}

const CHUNK: i32 = rmc_world::coords::CHUNK_SIZE;

#[cfg(test)]
mod tests {
    use super::*;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    fn lab_tables() -> EcoTables {
        let shipped = EcoTables::load(ASSETS).unwrap();
        let src = with_forms(
            &EcoSources::read(ASSETS).unwrap(),
            &read_forms(ASSETS).unwrap(),
        )
        .unwrap();
        EcoTables::from_sources(Arc::clone(shipped.materials()), &src).unwrap()
    }

    #[test]
    fn the_lab_grows_each_lab_species_the_same_way_twice() {
        let t = lab_tables();
        for f in read_forms(ASSETS).unwrap() {
            let a = grow(&t, &f.species, 7, 4, 2).unwrap();
            assert_eq!(a.len(), 8, "a frame per flush");
            let last = &a.last().unwrap().graph;
            assert!(last.len() > 8, "{} grew {} metamers", f.species, last.len());
            assert_eq!(a, grow(&t, &f.species, 7, 4, 2).unwrap());
        }
    }

    #[test]
    fn the_lab_refuses_a_tree_that_is_not_a_graph() {
        let t = EcoTables::load(ASSETS).unwrap();
        let tree = t
            .plants()
            .iter()
            .find(|p| {
                p.parts
                    .as_ref()
                    .is_some_and(|p| matches!(p.form, FormDef::Tree { .. }))
            })
            .unwrap();
        assert!(matches!(
            grow(&t, &tree.key, 7, 1, 1),
            Err(LabError::NotGraph(_))
        ));
        assert!(matches!(
            grow(&t, "no such tree", 7, 1, 1),
            Err(LabError::NoSpecies(_))
        ));
    }
}
