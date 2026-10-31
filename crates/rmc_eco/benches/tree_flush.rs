//! docs/TREES.md T8 bench `tree_flush`: one growth flush
//! ([`rmc_eco::trees::grow`]) of the shipped oak after ten lab years
//! (about 3,600 metamers), in open light, paying for a full root's
//! biomass. Every iteration grows the same graph; the result is dropped
//! outside the timed region.

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use rmc_eco::EcoTables;
use rmc_eco::tree_lab;
use rmc_eco::trees::{Growth, Site, grow};
use rmc_world::coords::CellPos;

const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
const SEED: u64 = 1;
const YEARS: u32 = 10;
const FLUSHES: u32 = 4;
/// The open-sky light the lab's light pass gives every cell above ground.
const LIGHT: i32 = 255;

fn tree_flush(c: &mut Criterion) {
    let tables = EcoTables::load(ASSETS).expect("the shipped tables load");
    let seasons = YEARS * tables.seasons().seasons.len() as u32;
    let frames = tree_lab::grow(&tables, "oak", SEED, seasons, FLUSHES).expect("the lab grows");
    let graph = frames.last().expect("a frame").graph.clone();
    let oak = tables.plants().iter().find(|p| p.key == "oak").unwrap();
    let parts = oak.parts.as_ref().unwrap();
    let growth = Growth::from_form(&parts.form).expect("the oak grows as a graph");
    let budget = usize::from(u8::MAX / parts.cost.max(1));
    let site = Site {
        root: CellPos::new(0, 0, 0),
        light: |c: CellPos| if c.0.y > 0 { LIGHT } else { 0 },
        passable: |c: CellPos| c.0.y > 0,
    };
    c.bench_function("tree_flush", |b| {
        b.iter_batched(
            || (),
            |()| grow(&graph, &site, &growth, budget, 0, SEED),
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, tree_flush);
criterion_main!(benches);
