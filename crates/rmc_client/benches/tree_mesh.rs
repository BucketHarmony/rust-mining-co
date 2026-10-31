//! docs/TREES.md T8 bench `tree_mesh`: [`tree_mesh`] of the shipped oak
//! after ten lab years (about 3,600 metamers), in leaf. The client rebuilds
//! a tree's mesh whenever its graph changes, so this is the cost of one
//! tree's growth flush on screen.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use rmc_client::treemesh::tree_mesh;
use rmc_eco::EcoTables;
use rmc_eco::tree_lab;

const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
const SEED: u64 = 1;
const YEARS: u32 = 10;
const FLUSHES: u32 = 4;
/// Summer: the oak in full leaf.
const SEASON: u32 = 1;

fn mesh_tree(c: &mut Criterion) {
    let tables = EcoTables::load(ASSETS).expect("the shipped tables load");
    let seasons = YEARS * tables.seasons().seasons.len() as u32;
    let frames = tree_lab::grow(&tables, "oak", SEED, seasons, FLUSHES).expect("the lab grows");
    let graph = &frames.last().expect("a frame").graph;
    let index = tables.plants().iter().position(|p| p.key == "oak").unwrap();
    let look = &tables.plant_looks()[index];
    let g = look.graph.expect("the oak draws as a graph");
    let materials = tables.materials();
    let quads = tree_mesh(graph, look, &g, materials, SEASON).quad_count;
    assert!(quads > 0, "the oak has no faces");

    c.bench_function("tree_mesh", |b| {
        b.iter(|| black_box(tree_mesh(black_box(graph), look, &g, materials, SEASON)))
    });
}

criterion_group!(benches, mesh_tree);
criterion_main!(benches);
