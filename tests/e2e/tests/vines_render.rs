//! Vines climb a wall from their seeds, flower and fruit in their seasons,
//! and lie flat on the stone they climb (docs/VEGETATION.md V6).
//!
//! A flat, fed loam bed against a stone wall, with one seed of each vine
//! species at the wall's foot, grown and given biomass so a season covers
//! the wall.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the code set up
//! (each vine has a stem root and vine cells on the wall, the vine leaves
//! are drawn as vines and not cubes, the client's plant looks name them, and
//! the mesh input recolors the deciduous vines in autumn only) and that the
//! frames render; the screenshots (summer, autumn, winter, and flowering
//! after the next spring, when the grown vines flower) are for the
//! playtester.

use bevy::prelude::*;
use rmc_client::mesher::{MeshInput, is_cube, is_vine};
use rmc_client::{ClientEco, GameCamera, MeshQueue, MeshStats, Replica, Weather};
use rmc_eco::parts::parts_of;
use rmc_eco::tables::FormDef;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::{LayerId, PartKind};
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

/// The cells between two vines along x, and the bed's margin around them.
const SPACING: i32 = 6;
const MARGIN: i32 = 4;
/// The wall's height above the bed.
const WALL: i32 = 8;
/// Loam depth under the bed, and what its loam holds.
const DEPTH: i32 = 3;
const FED: i32 = 200;

#[test]
#[ignore]
fn vines_render() {
    // The weather plugin follows the season, which the leaves' colors read.
    let mut t = TestApp::new(TestConfig {
        weather: true,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "vines_render", run);
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
    let path = e2e::save_screenshot(&image, &format!("vines_render-{label}"), t.frame());
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

fn count(t: &TestApp, root: CellPos, kind: PartKind) -> usize {
    parts_of(t.host_world(), root)
        .iter()
        .filter(|(_, k)| *k == kind)
        .count()
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let materials = eco.materials().clone();
    let (loam, stone) = (
        materials.id("loam").expect("loam"),
        materials.id("stone").expect("stone"),
    );
    // (seed, stem, leaf, deciduous, flowering) per vine species.
    let vines: Vec<(MaterialId, MaterialId, MaterialId, bool, bool)> = eco
        .plants()
        .iter()
        .filter_map(|p| {
            let parts = p.parts.as_ref()?;
            if !matches!(parts.form, FormDef::Vine { .. }) {
                return None;
            }
            Some((
                materials.id(&p.material)?,
                materials.id(&parts.wood)?,
                materials.id(&parts.leaf)?,
                parts.shed.is_some(),
                parts.flower.is_some(),
            ))
        })
        .collect();
    assert!(vines.len() >= 2, "vines in the tables");
    assert!(vines.iter().any(|v| v.4), "a flowering vine");
    assert!(vines.iter().any(|v| v.3) && vines.iter().any(|v| !v.3));
    for v in &vines {
        let def = materials.get(v.2).expect("a leaf material");
        assert!(is_vine(def) && !is_cube(def), "vine leaves lie flat");
    }

    // A flat bed in the middle of the world, open sky above it, and a wall
    // along its north side.
    let bounds = t.host_world().bounds();
    let (min, max) = (bounds.min.0, bounds.max.0);
    let (mx, mz) = ((min.x + max.x + 1) / 2, (min.z + max.z + 1) / 2);
    let x0 = mx - SPACING * (vines.len() as i32 - 1) / 2;
    let x1 = x0 + SPACING * (vines.len() as i32 - 1);
    let ground = i32::from(t.host_world().surface_y(mx, mz));
    let (lo, hi) = (
        CellPos::new(x0 - MARGIN, ground - DEPTH + 1, mz - MARGIN),
        CellPos::new(x1 + MARGIN, ground, mz + MARGIN),
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
    let wall_z = mz + 1;
    bed.push((
        EditOp::Set(stone),
        Shape::Box {
            min: CellPos::new(x0 - MARGIN + 1, ground + 1, wall_z),
            max: CellPos::new(x1 + MARGIN - 1, ground + WALL, wall_z),
        },
    ));
    t.apply_edits(&bed);

    // One seed of each at the wall's foot, grown, with a full biomass
    // budget.
    let y = ground + 1;
    let roots: Vec<CellPos> = (0..vines.len())
        .map(|i| CellPos::new(x0 + SPACING * i as i32, y, mz))
        .collect();
    let feed = |t: &mut TestApp| {
        for &root in &roots {
            set_layer(t, root, LayerId::Growth, i32::from(u8::MAX));
            set_layer(t, root, LayerId::Biomass, i32::from(u8::MAX));
        }
    };
    for (&root, vine) in roots.iter().zip(&vines) {
        t.apply_edits(&[(
            EditOp::Set(vine.0),
            Shape::Box {
                min: root,
                max: root,
            },
        )]);
    }
    feed(t);

    // Spring to summer: each seed ripens into its stem and climbs.
    t.advance_season();
    feed(t);
    for (&root, vine) in roots.iter().zip(&vines) {
        assert_eq!(
            t.host_world().get_cell(root),
            Some(vine.1),
            "ripened into its stem"
        );
        assert!(
            count(t, root, PartKind::Vine) > 0,
            "{root:?} climbs in summer"
        );
    }

    // What the code set up: the client's looks name each vine's leaves, and
    // summer keeps their table colors.
    let world = t.app().world();
    let client_eco = world.resource::<ClientEco>();
    assert_eq!(client_eco.plant_looks, eco.plant_looks());
    for vine in &vines {
        assert!(
            client_eco
                .plant_looks
                .iter()
                .any(|l| l.leaf == Some(vine.2))
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
    let summer = input(1).recolor;
    let autumn = input(2).recolor;
    for vine in &vines {
        assert!(!summer.contains_key(&vine.2), "summer keeps the color");
        assert_eq!(
            autumn.contains_key(&vine.2),
            vine.3,
            "only deciduous vines turn"
        );
    }

    let centre = Vec3::new(mx as f32 + 0.5, (y + 3) as f32, mz as f32 + 0.5) * CELL_SIZE_M;
    let eye = centre + Vec3::new(2.0, 8.0, -12.0) * CELL_SIZE_M;
    shoot(t, eye, centre, "summer");

    // Autumn: the deciduous vines turn.
    t.advance_season();
    t.steps(2);
    assert_eq!(
        t.app().world().resource::<Weather>().season,
        2,
        "the client's autumn"
    );
    shoot(t, eye, centre, "autumn");

    // Winter: the vines keep their cells on the wall.
    t.advance_season();
    for &root in &roots {
        assert!(count(t, root, PartKind::Vine) > 0, "{root:?} holds on");
        assert_eq!(count(t, root, PartKind::Flower), 0, "{root:?} no flowers");
    }
    shoot(t, eye, centre, "winter");

    // Through winter, then through the next spring: grown out, the
    // flowering vines open their flowers.
    t.advance_season();
    feed(t);
    t.advance_season();
    for (&root, vine) in roots.iter().zip(&vines) {
        let flowers = count(t, root, PartKind::Flower);
        eprintln!(
            "{root:?}: {} vine cells, {flowers} flowers",
            count(t, root, PartKind::Vine)
        );
        assert_eq!(flowers > 0, vine.4, "{root:?} flowers in spring");
    }
    shoot(t, eye, centre, "flowering");
}
