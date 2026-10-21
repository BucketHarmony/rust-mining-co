//! Magma meets water (SAD-P2 §12). A pool sits above a magma pocket, two
//! cells of stone between them. Digging a shaft through that stone lets
//! the pool fall onto the magma. Within one season, contact boiling
//! (§7.3 B7) turns the water that touches the magma into steam and the
//! magma into obsidian: `count(obsidian) > 0`, `ledger.boil_to_steam > 0`,
//! and the pool's fill falls by at least `boil_to_steam`. The playtester
//! judges the shot down the shaft.

use bevy::prelude::*;
use rmc_client::{GameCamera, MeshQueue, MeshStats};
use rmc_testkit::{Fixture, SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::shape::Shape;

/// Site offsets (`dx`, `dz`) of the magma pocket's footprint: inside the
/// pool's 12..=19, so the pool lies over all of it.
const POCKET: (i32, i32) = (14, 17);
/// Site offsets of the shaft's footprint, inside the pocket's.
const SHAFT: (i32, i32) = (15, 16);
/// Cells of stone between the pool's floor and the pocket: the shaft's
/// height.
const ROOF: i32 = 2;
/// Every season's air temperature, and the pool's before the season:
/// spring's in `assets/seasons.ron`, 10.0 °C.
const AIR_TEMP: i16 = 100;

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
fn magma_meets_water() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "magma_meets_water", run);
}

/// The site-offset square `lo..=hi` in x and z, at y `y0..=y1`.
fn square(site: CellAabb, (lo, hi): (i32, i32), y0: i32, y1: i32) -> CellAabb {
    let (x0, z0) = (site.min.0.x, site.min.0.z);
    CellAabb::new(
        CellPos::new(x0 + lo, y0, z0 + lo),
        CellPos::new(x0 + hi, y1, z0 + hi),
    )
}

fn shape(b: CellAabb) -> Shape {
    Shape::Box {
        min: b.min,
        max: b.max,
    }
}

/// Where the scenario builds: the fixture's pool, the magma pocket under
/// it and the shaft between them.
#[derive(Clone, Copy, Debug)]
struct Layout {
    pool: CellAabb,
    pocket: CellAabb,
    shaft: CellAabb,
    /// The pool with the shaft and the pocket: every cell the pool's water
    /// can reach.
    reach: CellAabb,
}

/// Builds the pool over the pocket, warms the pool and the stone over the
/// pocket to [`AIR_TEMP`], and sets every season to [`AIR_TEMP`] without
/// rain. Leaves the shaft undug.
fn prepare(t: &mut TestApp) -> Layout {
    let site = t.fixture(Fixture::Pool);
    let pool = site.feature;
    let floor = pool.min.0.y;
    // The pocket: one layer of magma under ROOF cells of stone, with the
    // fixture's stone below it too, so the only water it can meet is the
    // pool's.
    let pocket_y = floor - ROOF - 1;
    assert!(
        pocket_y > site.site.min.0.y,
        "the pocket has fixture stone under it"
    );
    let l = Layout {
        pool,
        pocket: square(site.site, POCKET, pocket_y, pocket_y),
        shaft: square(site.site, SHAFT, pocket_y + 1, floor - 1),
        reach: CellAabb::new(CellPos::new(pool.min.0.x, pocket_y, pool.min.0.z), pool.max),
    };

    let magma = t
        .eco_tables()
        .role_material("magma")
        .expect("assets/materials.ron has magma");
    // The ground this near the surface is below 0 °C (the §8 profile), so
    // a pool dug into it freezes in the first sub-step and never reaches
    // the pocket. Warm the pool and the roof first, as a pool in a thaw.
    let warm = CellAabb::new(
        CellPos::new(pool.min.0.x, pocket_y + 1, pool.min.0.z),
        pool.max,
    );
    t.apply_edits(&[
        (EditOp::Set(magma), shape(l.pocket)),
        (
            EditOp::SetLayer {
                layer: LayerId::Temperature,
                value: i32::from(AIR_TEMP),
            },
            shape(warm),
        ),
    ]);
    let side = u32::try_from(POCKET.1 - POCKET.0 + 1).unwrap();
    assert_eq!(
        t.count("magma", l.pocket),
        side * side,
        "the pocket is magma"
    );
    assert_eq!(t.count("obsidian", l.reach), 0, "no obsidian yet");
    assert_eq!(t.temp(l.pool.min), AIR_TEMP, "the pool is warmed");
    assert!(
        t.temp(l.pocket.min) > AIR_TEMP,
        "the pocket is hot: {}",
        t.temp(l.pocket.min)
    );
    assert_eq!(t.count("water", l.shaft), 0, "the stone keeps them apart");

    // No rain, so only boiling, evaporation and condensation change the
    // pool's fill; a mild season, so the pool stays liquid.
    t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: Some(AIR_TEMP),
    });
    l
}

/// What [`prepare`] and one season give with the shaft left undug.
struct Baseline {
    /// The world's hash before the season.
    hash: u64,
    /// The season's `boil_to_steam` and `magma_set`. The ledger is the
    /// whole world's, where water also boils and magma sets beside the
    /// generated chambers (SAD-P2 §8); this is that part.
    boil_to_steam: i64,
    magma_set: i64,
    /// How much fill the undug pool lost: evaporation alone.
    lost: u64,
}

fn baseline(bounds: CellAabb) -> Baseline {
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        render: false,
        ..TestConfig::default()
    });
    t.step();
    let l = prepare(&mut t);
    let hash = t.host_world().hash();
    let fill_before = t.sum_layer(LayerId::Moisture, l.reach);
    t.advance_season();
    assert_eq!(t.ledger().rain_in, 0, "no rain fell");
    assert_eq!(
        t.count("obsidian", l.reach),
        0,
        "without the shaft the pool never meets the pocket"
    );
    let fill_after = t.sum_layer(LayerId::Moisture, l.reach);
    Baseline {
        hash,
        boil_to_steam: t.ledger().boil_to_steam,
        magma_set: t.ledger().magma_set,
        lost: fill_before.saturating_sub(fill_after),
    }
}

fn run(t: &mut TestApp) {
    let base = baseline(t.config().world_bounds);
    eprintln!(
        "baseline: boil_to_steam {}, magma_set {} elsewhere in the world; the undug pool lost {}",
        base.boil_to_steam, base.magma_set, base.lost
    );
    settle(t);
    let l = prepare(t);
    let Layout {
        pool,
        pocket,
        shaft,
        reach,
    } = l;
    eprintln!("pool: {pool:?}, pocket: {pocket:?}, shaft: {shaft:?}");
    assert_eq!(
        t.host_world().hash(),
        base.hash,
        "the baseline built the same world"
    );

    // Dig through.
    let magma = t.count("magma", pocket);
    t.apply_edits(&[(EditOp::Remove, shape(shaft))]);
    assert_eq!(t.count("water", shaft), 0, "the shaft is dug out");
    assert_eq!(t.count("magma", pocket), magma, "magma is not dug");
    let fill_before = t.sum_layer(LayerId::Moisture, reach);

    t.advance_season();
    let ledger = t.ledger().clone();
    assert_eq!(ledger.rain_in, 0, "no rain fell");
    let fill_after = t.sum_layer(LayerId::Moisture, reach);
    let obsidian = t.count("obsidian", reach);
    let boiled = ledger.boil_to_steam - base.boil_to_steam;
    eprintln!(
        "after one season: obsidian {obsidian} (pocket {}), magma {}, water in the shaft {}; \
         pool fill {fill_before} -> {fill_after}; ledger boil_to_steam {} ({boiled} over the \
         baseline), magma_set {}, evap_to_steam {}, condense_from_steam {}, steam_vent_out {}",
        t.count("obsidian", pocket),
        t.count("magma", pocket),
        t.count("water", shaft),
        ledger.boil_to_steam,
        ledger.magma_set,
        ledger.evap_to_steam,
        ledger.condense_from_steam,
        ledger.steam_vent_out,
    );
    assert!(obsidian > 0, "magma that met the water set to obsidian");
    assert!(
        t.count("obsidian", pocket) > 0,
        "the obsidian is in the pocket, under the shaft"
    );
    // Every pocket cell that set is a magma_set over the baseline's
    // (§7.3 B7 books one per contact boil).
    assert!(
        ledger.magma_set - base.magma_set >= i64::from(obsidian),
        "magma_set {} over the baseline's {} for {obsidian} obsidian cells",
        ledger.magma_set,
        base.magma_set
    );
    assert!(ledger.boil_to_steam > 0, "water boiled to steam");
    assert!(boiled > 0, "the pool's water boiled to steam");
    assert!(
        fill_after < fill_before,
        "the pool lost water: {fill_before} -> {fill_after}"
    );
    let lost = i64::try_from(fill_before - fill_after).unwrap();
    assert!(
        lost >= boiled,
        "the pool lost {lost}, less than the {boiled} of it that boiled"
    );
    assert!(
        lost > i64::try_from(base.lost).unwrap(),
        "the dug pool lost {lost}, no more than the undug pool's {}",
        base.lost
    );

    shoot(t, pool, shaft);
}

/// Saves a screenshot looking down the shaft from over the pool's rim.
fn shoot(t: &mut TestApp, pool: CellAabb, shaft: CellAabb) {
    t.steps(3);
    settle(t);
    {
        let (min, max) = (shaft.min.0, shaft.max.0);
        let target = Vec3::new(
            (min.x + max.x + 1) as f32 / 2.0,
            min.y as f32,
            (min.z + max.z + 1) as f32 / 2.0,
        ) * CELL_SIZE_M;
        let rim = (pool.max.0.y + 1) as f32 * CELL_SIZE_M;
        let eye = Vec3::new(target.x + 1.0, rim + 3.0, target.z);
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_translation(eye).looking_at(target, Vec3::Y);
    }
    t.steps(2);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, "magma_meets_water", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "render failure: {:?}",
        t.render_failure()
    );
}
