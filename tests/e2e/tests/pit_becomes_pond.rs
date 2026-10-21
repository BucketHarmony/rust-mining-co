//! A pit dug below the water table becomes a pond (SAD-P2 §12). With rain
//! 0 for one season, the pit holds water: the wet ground it was dug from
//! leaves its moisture in the pit (§7.0, "a wet pit is a puddle next
//! sweep"), and the porous walls below the table seep into it (§7.3 B4). A
//! control pit dug the same way but ending above the table stays dry. The
//! playtester judges the pond from the saved screenshot.

use bevy::prelude::*;
use rmc_client::{GameCamera, LiquidMesh, MeshQueue, MeshStats};
use rmc_testkit::{SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::shape::Shape;

/// Side of each pit's square footprint, in cells.
const PIT_SIZE: i32 = 8;
/// The pit's min corner in x and z: the middle of the 64-wide world.
const PIT_MIN: i32 = 28;
/// The control pit's min corner in x (same z): far enough
/// from the pit that they share no wall, and clear of the bedrock shell.
const CONTROL_MIN_X: i32 = 12;
/// How many cells under the lowest table of its columns the pit goes.
const PIT_BELOW_TABLE: i32 = 4;
/// How many cells over the highest table of its columns the control's
/// floor stays.
const CONTROL_ABOVE_TABLE: i32 = 4;

/// Steps until meshing is idle and nothing waits to upload.
fn settle(t: &mut TestApp) {
    for _ in 0..240 {
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

#[test]
#[ignore]
fn pit_becomes_pond() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "pit_becomes_pond", run);
}

/// The columns of the square footprint at `(x0, z0)`.
fn footprint(x0: i32, z0: i32) -> impl Iterator<Item = (i32, i32)> {
    (z0..z0 + PIT_SIZE).flat_map(move |z| (x0..x0 + PIT_SIZE).map(move |x| (x, z)))
}

/// The box from `bottom` up to the highest surface over the footprint at
/// `(x0, z0)`: what digging the pit removes.
fn pit_box(t: &TestApp, x0: i32, z0: i32, bottom: i32) -> CellAabb {
    let top = footprint(x0, z0)
        .map(|(x, z)| i32::from(t.host_world().surface_y(x, z)))
        .max()
        .expect("a footprint has columns");
    assert!(
        bottom <= top,
        "the pit's floor {bottom} is under the surface {top}"
    );
    CellAabb::new(
        CellPos::new(x0, bottom, z0),
        CellPos::new(x0 + PIT_SIZE - 1, top, z0 + PIT_SIZE - 1),
    )
}

/// `b` grown by one cell on every side but the top: the pit and its walls
/// and floor.
fn with_walls(b: CellAabb) -> CellAabb {
    CellAabb::new(
        CellPos::new(b.min.0.x - 1, b.min.0.y - 1, b.min.0.z - 1),
        CellPos::new(b.max.0.x + 1, b.max.0.y, b.max.0.z + 1),
    )
}

/// The part of `b` with y in `lo..=hi`.
fn band(b: CellAabb, lo: i32, hi: i32) -> CellAabb {
    CellAabb::new(
        CellPos::new(b.min.0.x, lo.max(b.min.0.y), b.min.0.z),
        CellPos::new(b.max.0.x, hi.min(b.max.0.y), b.max.0.z),
    )
}

fn run(t: &mut TestApp) {
    settle(t);
    let table = |t: &TestApp, x: i32, z: i32| {
        t.water_table_y(x, z)
            .expect("assets/worldgen.ron has a water table")
    };
    let pit_table = footprint(PIT_MIN, PIT_MIN)
        .map(|(x, z)| table(t, x, z))
        .min()
        .unwrap();
    let control_table = footprint(CONTROL_MIN_X, PIT_MIN)
        .map(|(x, z)| table(t, x, z))
        .max()
        .unwrap();
    let pit = pit_box(t, PIT_MIN, PIT_MIN, pit_table - PIT_BELOW_TABLE);
    let control = pit_box(
        t,
        CONTROL_MIN_X,
        PIT_MIN,
        control_table + CONTROL_ABOVE_TABLE,
    );
    eprintln!("pit: {pit:?} (table {pit_table}), control: {control:?} (table {control_table})");
    assert!(
        control.max.0.x + 1 < pit.min.0.x - 1,
        "the pits share no wall"
    );
    // The part of the pit and its walls below the table, and the control
    // with its walls: the ground both are dug from.
    let pit_wet = band(with_walls(pit), i32::MIN, pit_table - 1);
    let control_ground = with_walls(control);
    let wet_before = t.sum_layer(LayerId::Moisture, pit_wet);
    let dry_before = t.sum_layer(LayerId::Moisture, control_ground);
    eprintln!("moisture before digging: pit {wet_before}, control {dry_before}");
    assert!(wet_before > 0, "the ground under the table is wet");
    assert_eq!(dry_before, 0, "the ground over the table is dry");

    t.apply_edits(&[
        (
            EditOp::Remove,
            Shape::Box {
                min: pit.min,
                max: pit.max,
            },
        ),
        (
            EditOp::Remove,
            Shape::Box {
                min: control.min,
                max: control.max,
            },
        ),
    ]);
    // Both pits are open: no solid or liquid cell is left in them, so no
    // water either. The wet ground dug out under the table left its
    // moisture on the pit's air cells.
    for (name, b) in [("pit", pit), ("control", control)] {
        let world = t.host_world();
        let left = (b.min.0.y..=b.max.0.y)
            .flat_map(|y| {
                (b.min.0.z..=b.max.0.z)
                    .flat_map(move |z| (b.min.0.x..=b.max.0.x).map(move |x| (x, y, z)))
            })
            .filter(|&(x, y, z)| world.is_solid_or_liquid(CellPos::new(x, y, z)))
            .count();
        assert_eq!(left, 0, "the {name} is dug out");
    }
    let dug_fill = t.sum_layer(LayerId::Moisture, pit);
    eprintln!(
        "moisture on the dug cells: pit {dug_fill}, control {}",
        t.sum_layer(LayerId::Moisture, control)
    );

    // One season without rain.
    t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: None,
    });
    t.advance_season();
    assert_eq!(t.ledger().rain_in, 0, "no rain fell");
    let (in_pit, in_control) = (t.count("water", pit), t.count("water", control));
    let below = band(pit, i32::MIN, pit_table - 1);
    eprintln!(
        "after one season: water cells pit {in_pit} ({} below the table), control {in_control}; \
         fill pit {}, control {}; ledger evap_to_steam {}, steam_vent_out {}",
        t.count("water", below),
        t.sum_layer(LayerId::Moisture, pit),
        t.sum_layer(LayerId::Moisture, control),
        t.ledger().evap_to_steam,
        t.ledger().steam_vent_out,
    );
    assert!(in_pit > 0, "the pit dug below the water table holds water");
    assert!(
        t.count("water", below) > 0,
        "the pond lies below the water table"
    );
    assert_eq!(in_control, 0, "the pit over the table stays dry");
    // Without rain, the pond outgrows what digging left in the pit, after
    // evaporation: the ground below the table feeds it.
    let fill = t.sum_layer(LayerId::Moisture, pit);
    assert!(
        fill > dug_fill,
        "the pit's fill grew from {dug_fill} to {fill} in a season without rain"
    );

    shoot(t, pit);
}

/// Saves a screenshot looking down into the pit from over its rim; the
/// pond is on the liquid meshes.
fn shoot(t: &mut TestApp, pit: CellAabb) {
    t.steps(3);
    settle(t);
    {
        let (min, max) = (pit.min.0, pit.max.0);
        let centre = Vec3::new(
            (min.x + max.x + 1) as f32 / 2.0,
            min.y as f32,
            (min.z + max.z + 1) as f32 / 2.0,
        ) * CELL_SIZE_M;
        let rim = (max.y + 1) as f32 * CELL_SIZE_M;
        let eye = Vec3::new(centre.x + 1.0, rim + 3.0, centre.z);
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
    }
    t.steps(2);
    {
        let world = t.app_mut().world_mut();
        let quads: u32 = world
            .query::<&LiquidMesh>()
            .iter(world)
            .map(|m| m.quads)
            .sum();
        assert!(quads > 0, "the pond is meshed");
    }

    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, "pit_becomes_pond", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "render failure: {:?}",
        t.render_failure()
    );
}
