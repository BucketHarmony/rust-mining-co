//! SAD-P2 §13 bench `tick_first`: the first season tick after worldgen,
//! with the shipped season 0 rain, on the default 32-chunk world. Target
//! < 5 s in release.
//!
//! Every iteration ticks a fresh copy of the generated world; copying and
//! dropping it are outside the timed region.

mod common;

use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use rmc_eco::bench_support;

fn tick_first(c: &mut Criterion) {
    // Built on the first call, so a filtered run skips it.
    let mut setup = None;
    c.bench_function("tick_first", |b| {
        let (tables, world) = setup.get_or_insert_with(|| {
            let tables = common::tables();
            let world = common::generate(&tables, common::DEFAULT_CHUNKS, common::SEED);
            (tables, world)
        });
        b.iter_batched(
            || world.clone(),
            |mut w| {
                let r = bench_support::tick_first(&mut w, tables, common::SEED);
                (w, r)
            },
            BatchSize::PerIteration,
        )
    });
}

criterion_group! {
    name = benches;
    // One iteration took about 11 s (F172); criterion's minimum sample
    // count, with room for all ten.
    config = Criterion::default().sample_size(10).measurement_time(Duration::from_secs(120));
    targets = tick_first
}
criterion_main!(benches);
