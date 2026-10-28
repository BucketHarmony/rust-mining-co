//! Plants draw as crossed quads that show their stage, health and season
//! (docs/VEGETATION.md V3).
//!
//! A flat loam bed with a row of each species; the left half of the bed is
//! fed (n, p and k), the right half bare, and growth ramps up along x.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the code set up
//! (the client's plant looks come from the tables, the chunk material sways
//! at `SWAY_RATE`, the mesh input carries a plant cell for every plant with
//! a starved plant colored apart from a fed one) and that the frames render;
//! the screenshots (spring, then two seasons on) are for the playtester.

use bevy::prelude::*;
use rmc_client::material::{ChunkEmissiveMaterial, EmissiveChunkMaterial, SWAY_RATE};
use rmc_client::mesher::MeshInput;
use rmc_client::{ClientEco, GameCamera, MeshQueue, MeshStats, Replica, Weather};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

/// The bed's size in cells along x, and the rows per species along z.
const WIDTH: i32 = 12;
const ROWS: i32 = 2;
/// Loam depth under the bed, and what the fed half's loam holds.
const DEPTH: i32 = 3;
const FED: i32 = 200;

#[test]
#[ignore]
fn plants_render() {
    // The weather plugin follows the season, which the plants' colors read.
    let mut t = TestApp::new(TestConfig {
        weather: true,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "plants_render", run);
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
    let path = e2e::save_screenshot(&image, &format!("plants_render-{label}"), t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None, "{label} renders");
}

fn run(t: &mut TestApp) {
    settle(t);
    let eco = t.eco_tables().clone();
    let materials = eco.materials().clone();
    let loam = materials.id("loam").expect("loam");
    let species: Vec<MaterialId> = eco
        .plants()
        .iter()
        .map(|p| materials.id(&p.material).expect("a plant material"))
        .collect();
    let depth = ROWS * species.len() as i32;

    // A flat bed in the middle of the world, air above it.
    let bounds = t.host_world().bounds();
    let (min, max) = (bounds.min.0, bounds.max.0);
    // Just past the middle, so the bed lies in one chunk.
    let (x0, z0) = ((min.x + max.x + 1) / 2 + 2, (min.z + max.z + 1) / 2 + 2);
    let (x1, z1) = (x0 + WIDTH - 1, z0 + depth - 1);
    let ground = i32::from(t.host_world().surface_y(x0, z0));
    t.apply_edits(&[
        (
            EditOp::Set(MaterialId::AIR),
            Shape::Box {
                min: CellPos::new(x0, ground + 1, z0),
                max: CellPos::new(x1, ground + 6, z1),
            },
        ),
        (
            EditOp::Set(loam),
            Shape::Box {
                min: CellPos::new(x0, ground - DEPTH + 1, z0),
                max: CellPos::new(x1, ground, z1),
            },
        ),
    ]);
    // The left half fed, the right half bare; water in all of it.
    let half = x0 + WIDTH / 2 - 1;
    let mut soil = Vec::new();
    for key in ["n", "p", "k"] {
        let x = eco.substance_index(key).expect("a plant nutrient");
        soil.push((
            EditOp::SetLayer {
                layer: LayerId::Substance(x),
                value: FED,
            },
            Shape::Box {
                min: CellPos::new(x0, ground - DEPTH + 1, z0),
                max: CellPos::new(half, ground, z1),
            },
        ));
    }
    soil.push((
        EditOp::SetLayer {
            layer: LayerId::Moisture,
            value: 48,
        },
        Shape::Box {
            min: CellPos::new(x0, ground - DEPTH + 1, z0),
            max: CellPos::new(x1, ground, z1),
        },
    ));
    t.apply_edits(&soil);

    // ROWS rows of each species, growth ramping up along x.
    let y = ground + 1;
    let mut planted = Vec::new();
    for (i, &m) in species.iter().enumerate() {
        for row in 0..ROWS {
            let z = z0 + ROWS * i as i32 + row;
            planted.push((
                EditOp::Set(m),
                Shape::Box {
                    min: CellPos::new(x0, y, z),
                    max: CellPos::new(x1, y, z),
                },
            ));
        }
    }
    t.apply_edits(&planted);
    for x in x0..=x1 {
        let growth = (x - x0 + 1) * i32::from(u8::MAX) / WIDTH;
        t.apply_edits(&[(
            EditOp::SetLayer {
                layer: LayerId::Growth,
                value: growth,
            },
            Shape::Box {
                min: CellPos::new(x, y, z0),
                max: CellPos::new(x, y, z1),
            },
        )]);
    }
    settle(t);

    // What the code set up: the looks from the tables, the sway rate, and a
    // plant cell for every plant with health in its color.
    let world = t.app().world();
    let client_eco = world.resource::<ClientEco>();
    assert_eq!(client_eco.plant_looks, eco.plant_looks());
    assert!(client_eco.plant_looks.iter().any(|l| l.sway > 0));
    assert!(!client_eco.deficiency.is_empty());
    let handle = &world.resource::<EmissiveChunkMaterial>().0;
    let material = world
        .resource::<Assets<ChunkEmissiveMaterial>>()
        .get(handle)
        .expect("the chunk material exists");
    assert_eq!(material.extension.sway_rate, SWAY_RATE);

    let replica = world.resource::<Replica>().world();
    let row_of = |m: MaterialId| species.iter().position(|&s| s == m).unwrap() as i32;
    let fed_cell = CellPos::new(x0 + 4, y, z0 + ROWS * row_of(species[species.len() - 1]));
    let bare_cell = CellPos::new(x1 - 1, y, fed_cell.0.z);
    let (pos, _) = fed_cell.split();
    assert_eq!(bare_cell.split().0, pos, "both in one chunk");
    let input = MeshInput::from_world(replica, pos)
        .expect("resident")
        .with_plants(replica, &client_eco.plant_looks, &client_eco.deficiency, 0);
    let in_chunk = |c: &CellPos| c.split().0 == pos;
    let plants_here = (x0..=x1)
        .flat_map(|x| (z0..=z1).map(move |z| CellPos::new(x, y, z)))
        .filter(in_chunk)
        .filter(|&c| replica.get_cell(c).is_some_and(|m| species.contains(&m)))
        .count();
    assert_eq!(input.plants.len(), plants_here, "a plant cell per plant");
    let cell_of = |c: CellPos| {
        let local = c.0 - pos.origin_cell().0;
        input.plants[&MeshInput::index(local.x, local.y, local.z)]
    };
    let (fed, bare) = (cell_of(fed_cell), cell_of(bare_cell));
    assert_ne!(fed.color, bare.color, "the bare soil's plant looks starved");
    assert!(bare.stage > fed.stage, "growth ramps along x");

    // From the bare side's front corner, looking across the bed.
    let centre = Vec3::new(
        (x0 + x1 + 1) as f32 / 2.0,
        y as f32,
        (z0 + z1 + 1) as f32 / 2.0,
    ) * CELL_SIZE_M;
    let eye = centre + Vec3::new(0.0, 4.0, -(depth as f32) * 0.9) * CELL_SIZE_M;
    shoot(t, eye, centre, "spring");

    // Two seasons on: autumn's colors.
    t.advance_season();
    t.advance_season();
    t.steps(2);
    assert_eq!(
        t.app().world().resource::<Weather>().season,
        2,
        "the client's autumn"
    );
    shoot(t, eye, centre, "later");
}
