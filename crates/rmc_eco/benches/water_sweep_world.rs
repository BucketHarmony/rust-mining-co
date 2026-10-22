//! SAD-P2 §13 bench `water_sweep_world`: one water flow sweep (§7.3 B) over
//! the default 32-chunk world as worldgen leaves it, every water holder in
//! the active set (as the host installs a world).
//!
//! Every iteration sweeps a fresh copy of the installed world; copying and
//! dropping it are outside the timed region.

mod common;

use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use rmc_eco::{WaterPass, bench_support};

fn water_sweep_world(c: &mut Criterion) {
    // Built on the first call, so a filtered run skips it.
    let mut setup = None;
    c.bench_function("water_sweep_world", |b| {
        let (tables, world) = setup.get_or_insert_with(|| {
            let tables = common::tables();
            let mut world = common::generate(&tables, common::DEFAULT_CHUNKS, common::SEED);
            bench_support::install(&mut world, &tables);
            (tables, world)
        });
        b.iter_batched(
            || (world.clone(), WaterPass::new(tables)),
            |(mut w, mut water)| {
                let r = bench_support::water_sweep_world(&mut w, &mut water);
                (w, r)
            },
            BatchSize::PerIteration,
        )
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20).measurement_time(Duration::from_secs(20));
    targets = water_sweep_world
}
criterion_main!(benches);
