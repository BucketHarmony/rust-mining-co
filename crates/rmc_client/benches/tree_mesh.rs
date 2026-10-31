//! docs/TREES.md T8 bench `tree_mesh`: [`tree_mesh`] of the shipped oak
//! after ten lab years, in leaf. The client rebuilds a tree's mesh whenever
//! its graph changes, so this is the cost of one tree's growth flush on
//! screen. `tree_mesh_pine` (T9) is the same for the pine, whose leaves
//! draw as needle rings.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use rmc_client::treemesh::tree_mesh;
use rmc_eco::EcoTables;
use rmc_eco::tree_lab;

const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
const SEED: u64 = 1;
const YEARS: u32 = 10;
const FLUSHES: u32 = 4;
/// Summer: in full leaf.
const SEASON: u32 = 1;

fn mesh_tree(c: &mut Criterion) {
    let tables = EcoTables::load(ASSETS).expect("the shipped tables load");
    for (name, species) in [("tree_mesh", "oak"), ("tree_mesh_pine", "pine")] {
        bench_species(c, &tables, name, species);
    }
}

/// `name`: the mesh of `species` after [`YEARS`] lab years.
fn bench_species(c: &mut Criterion, tables: &EcoTables, name: &str, species: &str) {
    let seasons = YEARS * tables.seasons().seasons.len() as u32;
    let frames = tree_lab::grow(tables, species, SEED, seasons, FLUSHES).expect("the lab grows");
    let graph = &frames.last().expect("a frame").graph;
    let index = tables
        .plants()
        .iter()
        .position(|p| p.key == species)
        .unwrap();
    let look = &tables.plant_looks()[index];
    let g = look.graph.expect("the species draws as a graph");
    let materials = tables.materials();
    let quads = tree_mesh(graph, look, &g, materials, SEASON).quad_count;
    assert!(quads > 0, "the {species} has no faces");
    eprintln!("{species}: {} metamers, {quads} quads", graph.len());

    c.bench_function(name, |b| {
        b.iter(|| black_box(tree_mesh(black_box(graph), look, &g, materials, SEASON)))
    });
}

criterion_group!(benches, mesh_tree);
criterion_main!(benches);
