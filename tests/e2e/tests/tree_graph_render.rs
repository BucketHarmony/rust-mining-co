//! Graph trees (docs/TREES.md T4): the host grows each lab species as a
//! metamer graph through the seasons, and the client draws each as one mesh
//! of cylinders and leaf cards.
//!
//! The host and client run on the shipped tables with the tree lab's forms
//! (`assets/tree_lab.ron`), on a flat, fed loam bed with one seed of each
//! lab species, given biomass every season.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the code set up
//! (each tree has a graph on the host and the same graph on the client, one
//! `TreeMesh` per tree with a mesh, its vertex count that of
//! `treemesh::tree_mesh` for the replica's graph, the deciduous meshes
//! losing their leaf cards in the bare season and the evergreen keeping
//! them) and that the frames render; the screenshots (each turn of the
//! seasons, a close look, the evergreen) are for the playtester.
//!
//! A frame shows a turn between two seasons: the host's cells and graphs
//! as the last ticked season left them (its snow, its leaf drop) under the
//! client's weather and leaf colors for the next. Each season shot is
//! named `<ticked>-to-<shown>` for that. The `forest` shot stands at a
//! player's eye height before the densest stand of the trees worldgen
//! seeded around the bed, grown over the same seasons.

use std::sync::Arc;

use bevy::prelude::*;
use rmc_client::controller::EYE_HEIGHT_M;
use rmc_client::treemesh::{TreeMesh, tree_mesh};
use rmc_client::{ClientEco, GameCamera, MeshQueue, MeshStats, Replica, Weather};
use rmc_eco::EcoTables;
use rmc_eco::tables::{EcoSources, FormDef};
use rmc_eco::tree_lab;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos, LocalPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::materials::{MaterialId, MaterialTable};
use rmc_world::shape::Shape;

const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

/// The cells between two trees along x, and the bed's margin around them.
const SPACING: i32 = 10;
const MARGIN: i32 = 5;
/// Loam depth under the bed, and what its loam holds.
const DEPTH: i32 = 3;
const FED: i32 = 200;
/// Cleared ground around the bed, in cells, so close shots see a floor
/// and not the cut walls of the terrain.
const SCENE: i32 = 12;
/// A forest shot: trees within this many cells of a root count as its
/// stand, and the eye stands this far from it.
const STAND: i32 = 12;
const FOREST_BACK: i32 = 10;
/// Open air kept above the bed.
const HEADROOM: i32 = 30;
/// Seasons grown before the first shot: four years, into the fifth
/// summer, so the pine has formed its cone (TREES T9).
const GROW: usize = 17;
/// How far back the camera stands from the row of trees, and how far
/// above the ground at its feet, in cells.
const BACK: i32 = 34;
const EYE_UP: i32 = 4;

#[test]
#[ignore]
fn tree_graph_render() {
    let materials =
        Arc::new(MaterialTable::load(format!("{ASSETS}/materials.ron")).expect("materials load"));
    let src = tree_lab::with_forms(
        &EcoSources::read(ASSETS).expect("eco sources"),
        &tree_lab::read_forms(ASSETS).expect("tree_lab.ron"),
    )
    .expect("lab forms apply");
    let eco = Arc::new(EcoTables::from_sources(materials, &src).expect("lab tables"));
    let mut t = TestApp::with_eco(
        TestConfig {
            weather: true,
            ..TestConfig::default()
        },
        eco,
    );
    e2e::run_scenario(&mut t, "tree_graph_render", run);
}

fn settle(t: &mut TestApp) {
    for _ in 0..480 {
        t.step();
        let world = t.app().world();
        let stats = world.resource::<MeshStats>();
        if stats.initial_load_done
            && world.resource::<MeshQueue>().is_idle()
            && stats.awaiting_upload == 0
        {
            return;
        }
    }
    panic!("meshing did not settle");
}

fn shoot(t: &mut TestApp, eye: Vec3, at: Vec3, label: &str) {
    t.steps(8);
    settle(t);
    {
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_translation(eye).looking_at(at, Vec3::Y);
    }
    t.steps(4);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("tree_graph_render-{label}"), t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None, "{label} renders");
}

/// The turn on screen: the host's last ticked season and the client's.
fn turn(t: &TestApp) -> String {
    let weather = t.app().world().resource::<Weather>();
    let rows = &t.eco_tables().seasons().seasons;
    let ticked = (weather.season as usize + rows.len() - 1) % rows.len();
    format!("{}-to-{}", rows[ticked].key, weather.key)
}

fn set_layer(t: &mut TestApp, cell: CellPos, layer: LayerId, value: i32) {
    t.apply_edits(&[(
        EditOp::SetLayer { layer, value },
        Shape::Box {
            min: cell,
            max: cell,
        },
    )]);
}

/// Per root: the client's tree mesh entity, checked against the replica's
/// graph. Returns each tree's vertex count and its leaf count.
fn meshes(t: &mut TestApp, roots: &[CellPos]) -> Vec<(usize, usize)> {
    let host: Vec<_> = roots
        .iter()
        .map(|&r| {
            t.host_world()
                .tree(r)
                .cloned()
                .expect("a graph on the host")
        })
        .collect();
    let world = t.app_mut().world_mut();
    let found: Vec<TreeMesh> = world
        .query::<(&TreeMesh, &Mesh3d)>()
        .iter(world)
        .map(|(m, _)| m.clone())
        .collect();
    let world = t.app().world();
    let replica = world.resource::<Replica>().world();
    let eco = world.resource::<ClientEco>();
    let season = world.resource::<Weather>().season;
    roots
        .iter()
        .zip(&host)
        .map(|(&root, host)| {
            let graph = replica.tree(root).expect("the graph replicates");
            assert_eq!(graph, host, "{root:?}: the replica's graph is the host's");
            let mesh = found
                .iter()
                .find(|m| m.root == root)
                .unwrap_or_else(|| panic!("{root:?} has a tree mesh"));
            assert_eq!(mesh.hash, graph.hash());
            let species = replica.plant(root).expect("a plant at the root").species;
            let look = &eco.plant_looks[usize::from(species)];
            let g = look.graph.expect("a graph look");
            let want = tree_mesh(graph, look, &g, replica.materials(), season);
            assert_eq!(mesh.vertices, want.positions.len());
            let leaves = graph.metamers().iter().map(|m| usize::from(m.leaf)).sum();
            (mesh.vertices, leaves)
        })
        .collect()
}

/// The roots of grown graph trees in the host world, but for `skip`.
fn wild_trees(t: &TestApp, skip: &[CellPos]) -> Vec<CellPos> {
    let world = t.host_world();
    let mut out = Vec::new();
    for pos in world.bounds().chunks() {
        let Some(chunk) = world.chunk(pos) else {
            continue;
        };
        for (&local, graph) in &chunk.trees {
            let root = CellPos::from_chunk_local(pos, LocalPos::from_index(local));
            if graph.len() > 8 && !skip.contains(&root) {
                out.push(root);
            }
        }
    }
    out.sort();
    out
}

/// A player's view of the densest wild stand: from [`FOREST_BACK`] cells
/// off its middle, on the side whose ground is nearest the stand's, at
/// eye height.
fn forest(t: &mut TestApp, bed: &[CellPos]) {
    let wild = wild_trees(t, bed);
    assert!(
        wild.len() >= 3,
        "worldgen's seeds grew {} trees",
        wild.len()
    );
    let near = |a: CellPos, b: CellPos| {
        let d = a.0 - b.0;
        d.x * d.x + d.z * d.z <= STAND * STAND
    };
    let heart = *wild
        .iter()
        .max_by_key(|&&a| {
            (
                wild.iter().filter(|&&b| near(a, b)).count(),
                std::cmp::Reverse(a),
            )
        })
        .expect("a wild tree");
    let stand: Vec<CellPos> = wild.iter().copied().filter(|&b| near(heart, b)).collect();
    let n = stand.len() as i32;
    let mid = stand.iter().fold(IVec3::ZERO, |s, c| s + c.0) / n;
    let bounds = t.host_world().bounds();
    let ground = |x: i32, z: i32| i32::from(t.host_world().surface_y(x, z));
    let (gx, gz) = (mid.x, mid.z);
    let eye_cell = [(0, -1), (0, 1), (-1, 0), (1, 0)]
        .into_iter()
        .map(|(dx, dz)| {
            let x = (gx + dx * FOREST_BACK).clamp(bounds.min.0.x, bounds.max.0.x);
            let z = (gz + dz * FOREST_BACK).clamp(bounds.min.0.z, bounds.max.0.z);
            (x, z)
        })
        .min_by_key(|&(x, z)| (ground(x, z) - mid.y).abs())
        .expect("four sides");
    let feet = (ground(eye_cell.0, eye_cell.1) + 1) as f32 * CELL_SIZE_M;
    let eye = Vec3::new(
        (eye_cell.0 as f32 + 0.5) * CELL_SIZE_M,
        feet + EYE_HEIGHT_M,
        (eye_cell.1 as f32 + 0.5) * CELL_SIZE_M,
    );
    let at = Vec3::new(
        (gx as f32 + 0.5) * CELL_SIZE_M,
        mid.y as f32 * CELL_SIZE_M + 2.0,
        (gz as f32 + 0.5) * CELL_SIZE_M,
    );
    eprintln!("forest: {n} trees around {heart:?}, eye {eye}");
    shoot(t, eye, at, "forest");
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let materials = eco.materials().clone();
    let loam = materials.id("loam").expect("loam");
    // (seed, deciduous) per graph species.
    let trees: Vec<(MaterialId, bool)> = eco
        .plants()
        .iter()
        .filter_map(|p| {
            let parts = p.parts.as_ref()?;
            if !matches!(parts.form, FormDef::Graph { .. }) {
                return None;
            }
            Some((materials.id(&p.material)?, parts.shed.is_some()))
        })
        .collect();
    assert!(trees.len() >= 2, "graph trees in the lab tables");
    assert!(trees.iter().any(|t| t.1) && trees.iter().any(|t| !t.1));

    // A flat bed in the middle of the world, open sky above it.
    let bounds = t.host_world().bounds();
    let (min, max) = (bounds.min.0, bounds.max.0);
    let (mx, mz) = ((min.x + max.x + 1) / 2, (min.z + max.z + 1) / 2);
    let x0 = mx - SPACING * (trees.len() as i32 - 1) / 2;
    let ground = i32::from(t.host_world().surface_y(mx, mz));
    let (lo, hi) = (
        CellPos::new(x0 - MARGIN, ground - DEPTH + 1, mz - MARGIN),
        CellPos::new(
            x0 + SPACING * (trees.len() as i32 - 1) + MARGIN,
            ground,
            mz + MARGIN,
        ),
    );
    let top = (ground + HEADROOM).min(max.y);
    let clamp = |x: i32, lo: i32, hi: i32| x.clamp(lo, hi);
    let (s_lo, s_hi) = (
        (
            clamp(lo.0.x - SCENE, min.x, max.x),
            clamp(lo.0.z - SCENE, min.z, max.z),
        ),
        (
            clamp(hi.0.x + SCENE, min.x, max.x),
            clamp(hi.0.z + SCENE, min.z, max.z),
        ),
    );
    let mut bed = vec![
        (
            EditOp::Set(MaterialId::AIR),
            Shape::Box {
                min: CellPos::new(s_lo.0, ground + 1, s_lo.1),
                max: CellPos::new(s_hi.0, top, s_hi.1),
            },
        ),
        (
            EditOp::Set(loam),
            Shape::Box {
                min: CellPos::new(s_lo.0, lo.0.y, s_lo.1),
                max: CellPos::new(s_hi.0, ground, s_hi.1),
            },
        ),
        (
            EditOp::Set(MaterialId::AIR),
            Shape::Box {
                min: CellPos::new(lo.0.x, ground + 1, lo.0.z),
                max: CellPos::new(hi.0.x, top, hi.0.z),
            },
        ),
        (EditOp::Set(loam), Shape::Box { min: lo, max: hi }),
        (
            EditOp::SetLayer {
                layer: LayerId::Moisture,
                value: 48,
            },
            Shape::Box { min: lo, max: hi },
        ),
    ];
    for key in ["n", "p", "k"] {
        let x = eco.substance_index(key).expect("a plant nutrient");
        bed.push((
            EditOp::SetLayer {
                layer: LayerId::Substance(x),
                value: FED,
            },
            Shape::Box { min: lo, max: hi },
        ));
    }
    t.apply_edits(&bed);

    let y = ground + 1;
    let roots: Vec<CellPos> = (0..trees.len())
        .map(|i| CellPos::new(x0 + SPACING * i as i32, y, mz))
        .collect();
    let feed = |t: &mut TestApp| {
        for &root in &roots {
            set_layer(t, root, LayerId::Growth, i32::from(u8::MAX));
            set_layer(t, root, LayerId::Biomass, i32::from(u8::MAX));
        }
    };
    for (&root, tree) in roots.iter().zip(&trees) {
        t.apply_edits(&[(
            EditOp::Set(tree.0),
            Shape::Box {
                min: root,
                max: root,
            },
        )]);
    }
    feed(t);
    for _ in 0..GROW {
        t.advance_season();
        feed(t);
    }
    t.steps(4);
    settle(t);
    let grown = meshes(t, &roots);
    for (&root, &(vertices, leaves)) in roots.iter().zip(&grown) {
        let graph = t.host_world().tree(root).unwrap();
        assert!(graph.len() > 8, "{root:?} grew {} metamers", graph.len());
        assert!(vertices > 0 && leaves > 0, "{root:?} is drawn in leaf");
    }

    let centre = Vec3::new(mx as f32 + 0.5, (y + 8) as f32, mz as f32 + 0.5) * CELL_SIZE_M;
    let (ez, ex) = ((mz - BACK).max(min.z), mx);
    let eye_y = (i32::from(t.host_world().surface_y(ex, ez)) + EYE_UP).max(y + 8);
    let eye = Vec3::new(ex as f32 + 0.5, eye_y as f32, ez as f32 + 0.5) * CELL_SIZE_M;
    let label = turn(t);
    shoot(t, eye, centre, &label);
    // Close under the first crown.
    let first = Vec3::new(roots[0].0.x as f32 + 0.5, y as f32, mz as f32 + 0.5) * CELL_SIZE_M;
    shoot(
        t,
        first + Vec3::new(2.5, 1.2, -3.5),
        first + Vec3::Y * 2.0,
        "close",
    );
    // Close beside the evergreen: its cone and needles (TREES T9).
    if let Some(i) = trees.iter().position(|t| !t.1) {
        let at = Vec3::new(roots[i].0.x as f32 + 0.5, y as f32, mz as f32 + 0.5) * CELL_SIZE_M;
        // From above the crown's base, so the floor fills the backdrop.
        shoot(
            t,
            at + Vec3::new(2.5, 5.0, -6.0),
            at + Vec3::Y * 2.0,
            "evergreen",
        );
    }
    forest(t, &roots);

    // On through the bare season: the deciduous trees drop their leaves,
    // the evergreen keeps them. `advance_season` ticks a season and the
    // client then shows the next, so the host's graph is the last ticked
    // season's.
    let rows = eco.seasons().seasons.len();
    for _ in 0..rows {
        t.advance_season();
        feed(t);
        t.steps(4);
        settle(t);
        let label = turn(t);
        let weather = t.app().world().resource::<Weather>();
        let ticked = (weather.season as usize + rows - 1) % rows;
        let now = meshes(t, &roots);
        let bare = eco
            .plant_looks()
            .iter()
            .any(|l| l.graph.is_some() && l.bare.get(ticked) == Some(&true));
        if bare {
            for ((_, leaves), tree) in now.iter().zip(&trees) {
                assert_eq!(*leaves == 0, tree.1, "bare through winter iff deciduous");
            }
            shoot(t, eye, centre, &label);
            return;
        }
        shoot(t, eye, centre, &label);
    }
    panic!("no bare season in a year");
}
