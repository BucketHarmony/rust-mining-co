//! SAD-P2 §13 bench `tick_quiet`: one season tick of a quiescent world
//! (rain 0, constant `air_temp`), the default 32-chunk world after
//! `bench_support::SETTLE_SEASONS` quiet seasons. Target < 1 s in release.
//!
//! The iterations go on ticking the same world, one season each, with the
//! activity kept across them as the host keeps it.

mod common;

use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use rmc_eco::bench_support;

fn tick_quiet(c: &mut Criterion) {
    // Built on the first call, so a filtered run skips it.
    let mut setup = None;
    c.bench_function("tick_quiet", |b| {
        let (quiet, world, activity, season) = setup.get_or_insert_with(|| {
            let tables = common::tables();
            let quiet = bench_support::quiet_tables(&tables);
            let mut world = common::generate(&tables, common::DEFAULT_CHUNKS, common::SEED);
            let mut activity = bench_support::install(&mut world, &quiet);
            let season = bench_support::settle(&mut world, &quiet, common::SEED, &mut activity);
            (quiet, world, activity, season)
        });
        b.iter(|| {
            let r = bench_support::tick_quiet(world, quiet, *season, common::SEED, activity);
            *season += 1;
            r
        })
    });
}

criterion_group! {
    name = benches;
    // One iteration took about 6.5 s (F172); criterion's minimum sample
    // count, with room for all ten.
    config = Criterion::default().sample_size(10).measurement_time(Duration::from_secs(70));
    targets = tick_quiet
}
criterion_main!(benches);
