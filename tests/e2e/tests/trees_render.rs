//! Trees grow from their seeds into wood, branches and leaves, and their
//! leaves turn with the season (docs/VEGETATION.md V5).
//!
//! A flat, fed loam bed with one seed of each tree species, grown and given
//! biomass so a season raises a crown.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the code set up
//! (each tree has a wood root and leaves, the client's plant looks name the
//! leaf materials, the mesh input recolors the deciduous leaves in autumn
//! only, and the bare season drops their leaves but not the pine's) and that
//! the frames render; the screenshots (summer, autumn, winter, and bare after winter) are for the
//! playtester.

use bevy::prelude::*;
use rmc_client::mesher::MeshInput;
use rmc_client::{ClientEco, GameCamera, MeshQueue, MeshStats, Replica, Weather};
use rmc_eco::parts::parts_of;
use rmc_eco::tables::FormDef;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::{LayerId, PartKind};
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

/// The cells between two trees along x, and the bed's margin around them.
const SPACING: i32 = 8;
const MARGIN: i32 = 4;
/// Loam depth under the bed, and what its loam holds.
const DEPTH: i32 = 3;
const FED: i32 = 200;

#[test]
#[ignore]
fn trees_render() {
    // The weather plugin follows the season, which the leaves' colors read.
    let mut t = TestApp::new(TestConfig {
        weather: true,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "trees_render", run);
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
    // The season change reaches the mesh queue a few frames after the
    // season's messages; let it, then wait for the re-mesh.
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
    let path = e2e::save_screenshot(&image, &format!("trees_render-{label}"), t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None, "{label} renders");
}

/// Sets `layer` to `value` at `cell`.
fn set_layer(t: &mut TestApp, cell: CellPos, layer: LayerId, value: i32) {
    t.apply_edits(&[(
        EditOp::SetLayer { layer, value },
        Shape::Box {
            min: cell,
            max: cell,
        },
    )]);
}

fn leaves(t: &TestApp, root: CellPos) -> usize {
    parts_of(t.host_world(), root)
        .iter()
        .filter(|(_, k)| *k == PartKind::Leaf)
        .count()
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let materials = eco.materials().clone();
    let loam = materials.id("loam").expect("loam");
    // (seed, wood, leaf, deciduous) per tree species.
    let trees: Vec<(MaterialId, MaterialId, MaterialId, bool)> = eco
        .plants()
        .iter()
        .filter_map(|p| {
            let parts = p.parts.as_ref()?;
            // Trees only: vines (V6) need a wall to climb.
            if !matches!(parts.form, FormDef::Tree { .. }) {
                return None;
            }
            Some((
                materials.id(&p.material)?,
                materials.id(&parts.wood)?,
                materials.id(&parts.leaf)?,
                parts.shed.is_some(),
            ))
        })
        .collect();
    assert!(trees.len() >= 2, "trees in the tables");
    assert!(trees.iter().any(|t| t.3) && trees.iter().any(|t| !t.3));

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
    let mut bed = vec![
        (
            EditOp::Set(MaterialId::AIR),
            Shape::Box {
                min: CellPos::new(lo.0.x, ground + 1, lo.0.z),
                max: CellPos::new(hi.0.x, ground + 16, hi.0.z),
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

    // One seed of each, grown, with a full biomass budget.
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

    // Spring to summer: each seed ripens into its wood and leafs out.
    t.advance_season();
    feed(t);
    for (&root, tree) in roots.iter().zip(&trees) {
        assert_eq!(
            t.host_world().get_cell(root),
            Some(tree.1),
            "ripened into wood"
        );
        assert!(leaves(t, root) > 0, "{root:?} has leaves in summer");
    }

    // What the code set up: the client's looks name each tree's leaves, and
    // summer keeps their table colors.
    let world = t.app().world();
    let client_eco = world.resource::<ClientEco>();
    assert_eq!(client_eco.plant_looks, eco.plant_looks());
    for tree in &trees {
        assert!(
            client_eco
                .plant_looks
                .iter()
                .any(|l| l.leaf == Some(tree.2))
        );
    }
    let replica = world.resource::<Replica>().world();
    let (pos, _) = roots[0].split();
    let input = |season: u32| {
        MeshInput::from_world(replica, pos)
            .expect("resident")
            .with_plants(
                replica,
                &client_eco.plant_looks,
                &client_eco.deficiency,
                season,
            )
    };
    assert!(
        input(1).recolor.is_empty(),
        "summer leaves keep their color"
    );
    let autumn = input(2).recolor;
    for tree in &trees {
        assert_eq!(
            autumn.contains_key(&tree.2),
            tree.3,
            "only deciduous leaves turn"
        );
    }

    let centre = Vec3::new(mx as f32 + 0.5, (y + 4) as f32, mz as f32 + 0.5) * CELL_SIZE_M;
    let eye =
        centre + Vec3::new(0.0, 3.0, -(SPACING as f32) * trees.len() as f32 * 0.8) * CELL_SIZE_M;
    shoot(t, eye, centre, "summer");

    // Autumn: the deciduous crowns turn.
    t.advance_season();
    t.steps(2);
    assert_eq!(
        t.app().world().resource::<Weather>().season,
        2,
        "the client's autumn"
    );
    shoot(t, eye, centre, "autumn");

    // Through autumn into winter, then through winter: the deciduous trees
    // stand bare, the evergreen keeps its leaves.
    t.advance_season();
    shoot(t, eye, centre, "winter");
    t.advance_season();
    for (&root, tree) in roots.iter().zip(&trees) {
        assert_eq!(
            leaves(t, root) == 0,
            tree.3,
            "{root:?} bare in winter iff deciduous"
        );
    }
    // From above: rain gathers (and freezes) in the dug bed.
    let above = centre + Vec3::new(0.0, 10.0, -(SPACING as f32) * 1.5) * CELL_SIZE_M;
    shoot(t, above, centre, "bare");
}
