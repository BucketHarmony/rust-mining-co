//! SAD §13/§14 bench `mesh_chunk`: `GreedyMesher` on one chunk (with its
//! one-cell border). Target < 2 ms in release.
//!
//! The chunk is the default world's surface chunk at the world's center
//! column, generated from the shipped `assets/worldgen.ron`: terrain, soil
//! layers and strata, so the mesher sees a realistic mix of faces.

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use rmc_client::{ChunkMesher, GreedyMesher, MeshInput};
use rmc_world::coords::{CellPos, ChunkPos};
use rmc_world::materials::MaterialTable;
use rmc_world::world::World;
use rmc_worldgen::{GenParams, Generator};

const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
const PARAMS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/worldgen.ron");
const SEED: u64 = 45;

fn mesh_chunk(c: &mut Criterion) {
    let table = Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"));
    let params = GenParams::load(PARAMS_PATH).expect("assets/worldgen.ron loads");
    let generator = Generator::new(SEED, &params, &table).expect("shipped params fit the table");
    let bounds = generator.bounds();
    let mid = (bounds.min.0 + bounds.max.0) / 2;
    let surface = CellPos::new(mid.x, generator.surface_height(mid.x, mid.z), mid.z);
    let pos = surface.chunk();

    // The chunk and its 26 neighbours, for the border.
    let mut world = World::new(table.clone(), bounds);
    let p = pos.0;
    let near: Vec<ChunkPos> = (-1..=1)
        .flat_map(|y| (-1..=1).flat_map(move |z| (-1..=1).map(move |x| (x, y, z))))
        .map(|(x, y, z)| ChunkPos::new(p.x + x, p.y + y, p.z + z))
        .filter(|&n| bounds.intersects_chunk(n))
        .collect();
    for (p, chunk) in generator.generate_chunks(near) {
        world
            .insert_chunk(p, chunk)
            .expect("generated chunk fits the world");
    }
    let input = MeshInput::from_world(&world, pos).expect("the surface chunk is resident");
    let quads = GreedyMesher.mesh(&input).quad_count;
    assert!(quads > 0, "the surface chunk at {pos:?} has no faces");

    c.bench_function("mesh_chunk", |b| {
        b.iter(|| black_box(GreedyMesher.mesh(black_box(&input))))
    });
}

criterion_group!(benches, mesh_chunk);
criterion_main!(benches);
