//! A contour swale on a slope catches the rain (SAD-P2 §12). With rain
//! high for two seasons, the swale holds at least `eco_test.swale_min`
//! water cells, and its columns hold more water than the same shape on the
//! open slope uphill, by cells and by fill. The playtester judges the pool
//! from the saved screenshot.

use bevy::prelude::*;
use rmc_client::{GameCamera, LiquidMesh, MeshQueue, MeshStats};
use rmc_testkit::eco::{FIXTURE_SIZE, SLOPE_RUN, SWALE_DEPTH};
use rmc_testkit::{Fixture, SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::shape::Shape;

/// The swale's contour, in cells above the slope's low end. High on the
/// slope, so the pond that collects at the low end of the closed fixture
/// basin stays below it.
const CONTOUR: i32 = 5;
/// How many steps of the slope the control sits uphill of the swale.
const CONTROL_STEPS_UP: i32 = 1;
/// Every season's rain: the wettest row of `assets/seasons.ron` (autumn).
const RAIN: u8 = 160;
/// The fixture site is a closed basin of impermeable soil that keeps every
/// drop. A sump under the slope's low end, the last SUMP_STEPS steps dug
/// out SUMP_DEPTH cells below the ground level, holds the runoff, so high
/// rain does not drown the slope and the swale with it.
const SUMP_STEPS: i32 = 2;
const SUMP_DEPTH: i32 = 40;
/// Seasons of rain.
const SEASONS: u32 = 2;

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
fn season_swale_fills() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "season_swale_fills", run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let site = t.fixture(Fixture::Swale(CONTOUR));
    let swale = site.feature;

    // Equal column regions: the swale's footprint from its floor up to
    // SWALE_DEPTH cells above the contour (the dug cells and the air over
    // them), and the same shape on the undug slope CONTROL_STEPS_UP steps
    // uphill (toward -x), raised with the surface: there the bottom
    // SWALE_DEPTH cells are the slope's soil and the rest the air over it.
    let swale_cols = CellAabb::new(
        swale.min,
        CellPos::new(swale.max.0.x, swale.max.0.y + SWALE_DEPTH, swale.max.0.z),
    );
    let (dx, dy) = (-CONTROL_STEPS_UP * SLOPE_RUN, CONTROL_STEPS_UP);
    let control = CellAabb::new(
        CellPos::new(
            swale_cols.min.0.x + dx,
            swale_cols.min.0.y + dy,
            swale_cols.min.0.z,
        ),
        CellPos::new(
            swale_cols.max.0.x + dx,
            swale_cols.max.0.y + dy,
            swale_cols.max.0.z,
        ),
    );
    eprintln!("swale: {swale:?}, swale columns: {swale_cols:?}, control: {control:?}");
    assert!(
        control.min.0.x >= site.site.min.0.x,
        "the control is on the site"
    );
    for x in [control.min.0.x, control.max.0.x] {
        assert_eq!(
            site.surface_y(x),
            swale.max.0.y + dy,
            "the control's surface is one step of the slope"
        );
    }
    // The sump, and the band of its inside level with the swale, which the
    // pond in it must never reach. The band leaves out the cells along the
    // sump's rim, down which runoff from the slope and from the terrain
    // around the site falls into it.
    let sump = CellAabb::new(
        CellPos::new(
            site.site.max.0.x - SUMP_STEPS * SLOPE_RUN + 1,
            site.ground_y - SUMP_DEPTH,
            site.site.min.0.z,
        ),
        CellPos::new(
            site.site.max.0.x,
            site.surface_y(site.site.max.0.x - SUMP_STEPS * SLOPE_RUN + 1),
            site.site.max.0.z,
        ),
    );
    assert!(
        sump.min.0.x > swale.max.0.x + 1,
        "the sump is downhill of the berm"
    );
    assert_eq!(site.site.max.0.x - site.site.min.0.x + 1, FIXTURE_SIZE);
    t.apply_edits(&[(
        EditOp::Remove,
        Shape::Box {
            min: sump.min,
            max: sump.max,
        },
    )]);
    let sump_at_swale = CellAabb::new(
        CellPos::new(sump.min.0.x + 1, swale.min.0.y, sump.min.0.z + 1),
        CellPos::new(sump.max.0.x - 1, swale.max.0.y, sump.max.0.z - 1),
    );
    eprintln!("sump: {sump:?}");

    assert_eq!(t.count("water", swale_cols), 0, "the swale starts dry");
    assert_eq!(t.count("water", control), 0, "the control starts dry");

    // Rain high, every season.
    t.set_season_params(SeasonOverride {
        rain: Some(RAIN),
        air_temp: None,
    });
    let swale_min = t.eco_tables().eco_test().swale_min;
    for season in 0..SEASONS {
        t.advance_season();
        assert!(t.ledger().rain_in > 0, "it rained in season {season}");
        let in_swale = t.count("water", swale);
        let (in_cols, in_control) = (t.count("water", swale_cols), t.count("water", control));
        let (fill_cols, fill_control) = (
            t.sum_layer(LayerId::Moisture, swale_cols),
            t.sum_layer(LayerId::Moisture, control),
        );
        eprintln!(
            "season {season}: water cells swale {in_swale}, swale columns {in_cols}, \
             control {in_control}; fill swale columns {fill_cols}, control {fill_control}"
        );
        assert!(
            in_swale >= swale_min,
            "season {season}: the swale holds {in_swale} water cells, want >= {swale_min}"
        );
        eprintln!(
            "season {season}: sump water cells {}, fill {}",
            t.count("water", sump),
            t.sum_layer(LayerId::Moisture, sump)
        );
        assert_eq!(
            t.count("water", sump_at_swale),
            0,
            "season {season}: the sump's pond stays below the swale"
        );
        assert!(
            in_cols >= in_control + swale_min,
            "season {season}: {in_cols} water cells in the swale's columns, \
             {in_control} on the open slope"
        );
        assert!(
            fill_cols > fill_control,
            "season {season}: fill {fill_cols} in the swale's columns, \
             {fill_control} on the open slope"
        );
    }
    assert_eq!(t.season_log().len(), SEASONS as usize);

    // The pool is meshed in the liquid pass. Rain sheets cover the open
    // slope too while it rains, so a second shot follows a dry season, in
    // which the swale keeps its pool and the sheets on the slope thin.
    shoot(t, swale, "season_swale_fills");
    t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: None,
    });
    t.advance_season();
    let in_swale = t.count("water", swale);
    let (in_cols, in_control) = (t.count("water", swale_cols), t.count("water", control));
    let (fill_cols, fill_control) = (
        t.sum_layer(LayerId::Moisture, swale_cols),
        t.sum_layer(LayerId::Moisture, control),
    );
    eprintln!(
        "dry season: water cells swale {in_swale}, swale columns {in_cols}, control {in_control}; fill swale columns {fill_cols}, control {fill_control}"
    );
    assert!(
        in_swale >= swale_min,
        "dry season: the swale keeps {in_swale} water cells, want >= {swale_min}"
    );
    assert!(
        fill_cols > fill_control,
        "dry season: fill {fill_cols} in the swale's columns, {fill_control} on the open slope"
    );
    shoot(t, swale, "season_swale_fills-dry");
}

/// Saves a screenshot of the swale from high above the slope downhill of
/// the berm, looking up the slope: the swale, its berm and the open slope
/// uphill are in frame. Water is on the liquid meshes.
fn shoot(t: &mut TestApp, swale: CellAabb, name: &str) {
    t.steps(3);
    settle(t);
    {
        let (min, max) = (swale.min.0, swale.max.0);
        let centre = Vec3::new(
            (min.x + max.x + 1) as f32 / 2.0,
            (max.y + 1) as f32,
            (min.z + max.z + 1) as f32 / 2.0,
        ) * CELL_SIZE_M;
        let eye = centre + Vec3::new(7.0, 6.0, 0.0);
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
        assert!(quads > 0, "{name}: the water is meshed");
    }

    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, name, t.frame());
    eprintln!("screenshot: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "{name}: render failure: {:?}",
        t.render_failure()
    );
}
