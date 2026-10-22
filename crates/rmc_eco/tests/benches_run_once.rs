//! The SAD-P2 §13 bench bodies of `rmc_eco::bench_support`, each run once on
//! a tiny generated world, and the wiring of the bench targets that time
//! them on the default world.

#[path = "../benches/common/mod.rs"]
mod common;

use rmc_eco::bench_support;
use rmc_eco::{Transport, WaterPass};

/// One chunk wide and deep, the default world's two chunks high.
const TINY: [i32; 3] = [1, 2, 1];

/// The §13 benches, each a `harness = false` target in `benches/`.
const BENCHES: [&str; 4] = [
    "tick_quiet",
    "tick_first",
    "water_sweep_world",
    "chem_transport_world",
];

#[test]
fn benches_run_once() {
    let tables = common::tables();
    let substeps = usize::from(tables.seasons().substeps);
    let generated = common::generate(&tables, TINY, common::SEED);

    // tick_first: the first season after worldgen, with the shipped rain.
    let mut world = generated.clone();
    let first = bench_support::tick_first(&mut world, &tables, common::SEED);
    assert_eq!(first.substep_ms.len(), substeps);
    assert!(first.ledger.rain_in > 0, "season 0 rains: {first:?}");
    assert_ne!(
        world.hash(),
        generated.hash(),
        "the first season changed nothing"
    );

    // tick_quiet: rain 0 and one constant air temperature, after the
    // settling seasons.
    let quiet = bench_support::quiet_tables(&tables);
    assert!(quiet.seasons().seasons.iter().all(|s| s.rain == 0));
    let air = quiet.seasons().seasons[0].air_temp;
    assert!(quiet.seasons().seasons.iter().all(|s| s.air_temp == air));
    assert_eq!(air, tables.seasons().seasons[0].air_temp);
    let mut world = generated.clone();
    let mut activity = bench_support::install(&mut world, &quiet);
    let season = bench_support::settle(&mut world, &quiet, common::SEED, &mut activity);
    assert_eq!(season, bench_support::SETTLE_SEASONS);
    let r = bench_support::tick_quiet(&mut world, &quiet, season, common::SEED, &mut activity);
    assert_eq!(r.substep_ms.len(), substeps);
    assert_eq!(r.ledger.rain_in, 0, "a quiet season does not rain");

    // water_sweep_world: one flow sweep over the installed world's water.
    let mut world = generated.clone();
    bench_support::install(&mut world, &tables);
    let mut water = WaterPass::new(&tables);
    let sweep = bench_support::water_sweep_world(&mut world, &mut water);
    assert!(sweep.visits > 0, "the water table is active");

    // chem_transport_world: the moves of one rainy sub-step's flow sweeps,
    // replayed over a world holding every substance.
    let mut world = generated.clone();
    bench_support::install(&mut world, &tables);
    bench_support::add_substances(&mut world, &tables, bench_support::SUBSTANCE_AMOUNT);
    let log = bench_support::rained_moves(&mut world, &tables, common::SEED);
    assert!(!log.is_empty(), "rain on the tiny world flows");
    let transport = Transport::new(&tables);
    let moved = bench_support::chem_transport_world(&mut world, &transport, &log);
    assert_eq!(moved.entries, log.len() as u64);
    assert!(moved.moved > 0, "the moves carry substances: {moved:?}");
}

/// Each bench is declared with `harness = false`, registers a criterion
/// benchmark of its name, and has a measured row in `docs/perf-p2.md`.
#[test]
fn benches_declared() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"));
    let read = |path: String| {
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
            .replace("\r\n", "\n")
    };
    let manifest = read(format!("{dir}/Cargo.toml")).replace(' ', "");
    let perf = read(format!("{dir}/../../docs/perf-p2.md"));
    for name in BENCHES {
        assert!(
            manifest.contains(&format!("[[bench]]\nname=\"{name}\"\nharness=false")),
            "Cargo.toml declares no harness = false [[bench]] {name}"
        );
        let source = read(format!("{dir}/benches/{name}.rs"));
        assert!(
            source.contains("criterion_main!"),
            "{name}.rs: no criterion_main!"
        );
        assert!(
            source.contains(&format!("\"{name}\"")),
            "{name}.rs registers no benchmark named {name:?}"
        );
        assert!(
            source.contains(&format!("bench_support::{name}")),
            "{name}.rs does not time bench_support::{name}"
        );
        let row = perf
            .lines()
            .find(|l| l.starts_with('|') && l.contains(&format!("`{name}`")))
            .unwrap_or_else(|| panic!("docs/perf-p2.md has no row for `{name}`"));
        assert!(
            row.chars().any(|c| c.is_ascii_digit()),
            "docs/perf-p2.md row for `{name}` records no measurement: {row}"
        );
    }
}
