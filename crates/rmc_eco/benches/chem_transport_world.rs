//! SAD-P2 §13 bench `chem_transport_world`: chemistry transport (§7.6)
//! replaying the moves of one rainy sub-step's flow sweeps over the default
//! 32-chunk world, every cell holding `bench_support::SUBSTANCE_AMOUNT` of
//! every substance.
//!
//! Every iteration replays onto a fresh copy of the world the sweeps left;
//! copying and dropping it are outside the timed region.

mod common;

use std::time::Duration;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use rmc_eco::{Transport, bench_support};

fn chem_transport_world(c: &mut Criterion) {
    // Built on the first call, so a filtered run skips it.
    let mut setup = None;
    c.bench_function("chem_transport_world", |b| {
        let (transport, world, log) = setup.get_or_insert_with(|| {
            let tables = common::tables();
            let mut world = common::generate(&tables, common::DEFAULT_CHUNKS, common::SEED);
            bench_support::install(&mut world, &tables);
            bench_support::add_substances(&mut world, &tables, bench_support::SUBSTANCE_AMOUNT);
            let log = bench_support::rained_moves(&mut world, &tables, common::SEED);
            (Transport::new(&tables), world, log)
        });
        b.iter_batched(
            || world.clone(),
            |mut w| {
                let r = bench_support::chem_transport_world(&mut w, transport, log);
                (w, r)
            },
            BatchSize::PerIteration,
        )
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(20).measurement_time(Duration::from_secs(20));
    targets = chem_transport_world
}
criterion_main!(benches);
