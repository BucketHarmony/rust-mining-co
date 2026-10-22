//! The world the SAD-P2 §13 benches and `tests/benches_run_once.rs` run on:
//! the shipped `assets/` tables and `worldgen.ron`, fitted to a box of
//! whole chunks from the origin, with every Phase 2 worldgen step
//! (temperature, water table, magma, light, plants; SAD-P2 §8), as the game
//! generates it.

use std::sync::Arc;

use rmc_eco::EcoTables;
use rmc_world::coords::CHUNK_SIZE;
use rmc_world::world::World;
use rmc_worldgen::params::WorldBounds;
use rmc_worldgen::{
    GenParams, Generator, LightInit, MagmaInit, PlantInit, TemperatureInit, WaterTableInit,
};

const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

/// SAD-P2 §2: the default Phase 2 world, 4 × 2 × 4 chunks (128 × 64 × 128
/// cells, 1 M cells).
#[allow(dead_code)]
pub const DEFAULT_CHUNKS: [i32; 3] = [4, 2, 4];

/// The game's default seed (`rmc_app::session::DEFAULT_SEED`).
pub const SEED: u64 = 1;

/// The shipped Phase 2 tables.
pub fn tables() -> Arc<EcoTables> {
    Arc::new(EcoTables::load(ASSETS).expect("the shipped Phase 2 tables load"))
}

/// The world of `chunks` chunks along x, y and z from the origin, generated
/// from `seed`. It carries no eco rules and no active sets yet
/// ([`rmc_eco::bench_support::install`] adds them, as the host does).
pub fn generate(tables: &EcoTables, chunks: [i32; 3], seed: u64) -> World {
    let params = GenParams::load(format!("{ASSETS}/worldgen.ron"))
        .expect("assets/worldgen.ron loads")
        .fit_to(WorldBounds {
            min: [0; 3],
            max: chunks.map(|n| n * CHUNK_SIZE - 1),
        })
        .expect("worldgen.ron fits the bench world");
    let generator = Generator::new(seed, &params, tables.materials())
        .expect("worldgen.ron fits the material table")
        .with_temperature(TemperatureInit::new(tables))
        .with_water_table(WaterTableInit::new(tables))
        .with_magma(MagmaInit::new(tables))
        .with_light(LightInit::new(tables))
        .with_plants(PlantInit::new(tables));
    let mut world = World::new(tables.materials().clone(), generator.bounds());
    for (pos, chunk) in generator.generate_world() {
        world
            .insert_chunk(pos, chunk)
            .expect("generated chunk fits the world");
    }
    world
}
