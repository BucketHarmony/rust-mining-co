//! SAD §13/§14 bench `gen_world`: the full default world (256 chunks) from
//! the shipped `assets/worldgen.ron`, generated in parallel. Target < 3 s
//! in release.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use rmc_world::materials::MaterialTable;
use rmc_worldgen::{GenParams, Generator};

const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
const PARAMS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/worldgen.ron");
const SEED: u64 = 45;

fn gen_world(c: &mut Criterion) {
    let table = MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads");
    let params = GenParams::load(PARAMS_PATH).expect("assets/worldgen.ron loads");
    c.bench_function("gen_world", |b| {
        b.iter(|| {
            let g = Generator::new(SEED, &params, &table).expect("shipped params fit the table");
            black_box(g.generate_world())
        })
    });
}

criterion_group! {
    name = benches;
    // One iteration takes about a second; criterion's minimum sample count
    // keeps a run under a minute.
    config = Criterion::default().sample_size(10).measurement_time(Duration::from_secs(30));
    targets = gen_world
}
criterion_main!(benches);
