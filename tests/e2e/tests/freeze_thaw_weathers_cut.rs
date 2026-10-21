//! A trench cut across a slope collects rain, and its wet walls weather
//! (SAD-P2 §7.5, §12). The trench fixture's walls and floor are lined with
//! the first stone that weathers to another material. Over three seasons
//! whose air temperature goes below 0 °C and back (autumn, winter, spring
//! in `assets/seasons.ron`), the wall cells beside the trench water gain
//! regolith and an unexposed control cell of the same material at the same
//! depth gains none. Within [`MAX_FURTHER_SEASONS`] more seasons a wall
//! cell turns into its `weathers_to` material. The playtester judges the
//! weathered cut from the saved screenshots.
//!
//! With the shipped thermal values, rock edges move heat only at a
//! difference of 25.6 °C (SAD-P2 §7.1 dead zone), so the season swing does
//! not take the lining across 0 °C and its freeze-thaw bit stays clear; the
//! run logs the count. The pond in the cut freezes over in the cold season.

use bevy::prelude::*;
use rmc_client::{GameCamera, MeshQueue, MeshStats};
use rmc_testkit::eco::{SLOPE_RUN, TRENCH_DEPTH};
use rmc_testkit::{Fixture, SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

/// The seasons of the freeze-thaw run: the row before the first one below
/// 0 °C, that row, and the row after it.
const FREEZE_THAW_SEASONS: usize = 3;
/// At most this many seasons after the freeze-thaw run until a wall cell
/// has converted.
const MAX_FURTHER_SEASONS: u32 = 16;
/// The control column's offset from the site's min corner: under the
/// slope's next step uphill of the trench, where the ground is one cell
/// higher, so every control cell has solid ground on all six faces.
const CONTROL_DX: i32 = 9;
const CONTROL_DZ: i32 = 15;
/// The fixture site is a closed basin of impermeable ground that also
/// takes the runoff of the terrain around it, so the seasons' rain would
/// drown the slope and the trench under a lake. A sump under the slope's
/// low end, its last SUMP_STEPS steps dug out SUMP_DEPTH cells below the
/// ground level, holds the runoff of the seasons up to and through the
/// freeze-thaw run (as in `season_swale_fills`).
const SUMP_STEPS: i32 = 2;
const SUMP_DEPTH: i32 = 40;

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
fn freeze_thaw_weathers_cut() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "freeze_thaw_weathers_cut", run);
}

/// The cells of `region`.
fn cells(region: CellAabb) -> impl Iterator<Item = CellPos> {
    let (min, max) = (region.min.0, region.max.0);
    (min.y..=max.y).flat_map(move |y| {
        (min.z..=max.z).flat_map(move |z| (min.x..=max.x).map(move |x| CellPos::new(x, y, z)))
    })
}

fn contains(region: CellAabb, c: CellPos) -> bool {
    c.0.cmpge(region.min.0).all() && c.0.cmple(region.max.0).all()
}

/// The host's material at `c`.
fn material(t: &TestApp, c: CellPos) -> MaterialId {
    t.host_world().get_cell(c).expect("a resident cell")
}

/// Whether `c` has a water face neighbor on the host.
fn by_water(t: &TestApp, c: CellPos, water: MaterialId) -> bool {
    [
        IVec3::X,
        IVec3::NEG_X,
        IVec3::Y,
        IVec3::NEG_Y,
        IVec3::Z,
        IVec3::NEG_Z,
    ]
    .into_iter()
    .any(|d| t.host_world().get_cell(CellPos(c.0 + d)) == Some(water))
}

fn run(t: &mut TestApp) {
    settle(t);
    let materials = t.host_world().materials().clone();
    // The wall material: the first stone that weathers into another
    // material, and what it weathers to.
    let wall = materials
        .with_tag("stone")
        .find(|d| d.weathering_rate > 0 && d.weathers_to.is_some())
        .expect("a stone that weathers to another material");
    let (wall_id, wall_key) = (wall.id, wall.key.clone());
    let weathered = materials
        .id(wall.weathers_to.as_deref().unwrap())
        .expect("weathers_to names a material");
    let water = t
        .eco_tables()
        .role_material("water")
        .expect("a water material");

    let site = t.fixture(Fixture::Trench);
    let trench = site.feature;
    let top = trench.max.0.y;
    // The lining: one cell around the trench's sides, ends and floor, up to
    // the cell below the trench's top row. Its downhill side is the slope's
    // next step, whose surface is that row, so the trench spills there.
    let lining = CellAabb::new(
        CellPos::new(trench.min.0.x - 1, top - TRENCH_DEPTH, trench.min.0.z - 1),
        CellPos::new(trench.max.0.x + 1, top - 1, trench.max.0.z + 1),
    );
    let (x0, z0) = (site.site.min.0.x, site.site.min.0.z);
    let control = CellAabb::new(
        CellPos::new(x0 + CONTROL_DX, lining.min.0.y, z0 + CONTROL_DZ),
        CellPos::new(x0 + CONTROL_DX, lining.max.0.y, z0 + CONTROL_DZ),
    );
    assert!(
        site.surface_y(control.min.0.x) > top,
        "the control column is under the next step uphill"
    );
    assert!(control.min.0.x < lining.min.0.x - 1);
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
    assert!(sump.min.0.x > lining.max.0.x + 1, "the sump is downhill");
    let boxed = |op, r: CellAabb| {
        (
            op,
            Shape::Box {
                min: r.min,
                max: r.max,
            },
        )
    };
    t.apply_edits(&[
        boxed(EditOp::Set(wall_id), lining),
        boxed(EditOp::Remove, trench),
        boxed(EditOp::Set(wall_id), control),
        boxed(EditOp::Remove, sump),
    ]);
    let walls: Vec<CellPos> = cells(lining).filter(|&c| !contains(trench, c)).collect();
    eprintln!(
        "wall {wall_key} -> {:?}; trench {trench:?}; lining {lining:?}; control {control:?}",
        materials.get(weathered).map(|d| &d.key)
    );
    for &c in walls
        .iter()
        .chain(cells(control).collect::<Vec<_>>().iter())
    {
        assert_eq!(material(t, c), wall_id, "{c:?} is lined");
        assert_eq!(t.layer_u8(c, LayerId::Regolith), 0, "{c:?} starts fresh");
    }
    assert_eq!(t.count("water", trench), 0, "the trench starts dry");

    // Run the seasons up to the row before the first one below 0 °C.
    let rows = t.eco_tables().seasons().seasons.clone();
    let freeze = rows
        .iter()
        .position(|r| r.air_temp < 0)
        .expect("a season below 0 °C");
    let len = rows.len();
    let first = (freeze + len - 1) % len;
    let run_rows: Vec<_> = (0..FREEZE_THAW_SEASONS)
        .map(|k| &rows[(first + k) % len])
        .collect();
    assert!(
        run_rows[0].air_temp > 0 && run_rows[1].air_temp < 0 && run_rows[2].air_temp > 0,
        "the run's air temperature crosses 0 °C twice: {run_rows:?}"
    );
    let season = |t: &TestApp| t.host().season().expect("the host runs seasons");
    while season(t) as usize % len != first {
        t.advance_season();
    }

    // The freeze-thaw run. Rain runs down the slope in a sheet several
    // cells deep, so the pond in the cut freezes at the top of its columns.
    let trench_cols = CellAabb::new(
        trench.min,
        CellPos::new(
            trench.max.0.x,
            t.host_world().bounds().max.0.y,
            trench.max.0.z,
        ),
    );
    let mut thawed = 0;
    let mut froze = false;
    for k in 0..FREEZE_THAW_SEASONS {
        let row = &rows[season(t) as usize % len];
        t.advance_season();
        let flipped = walls
            .iter()
            .filter(|&&c| {
                let (pos, local) = c.split();
                t.host_world()
                    .chunk(pos)
                    .is_some_and(|ch| ch.freeze_thaw.contains(local))
            })
            .count();
        thawed += flipped;
        let regolith: u64 = walls
            .iter()
            .map(|&c| u64::from(t.layer_u8(c, LayerId::Regolith)))
            .sum();
        eprintln!(
            "season {k} ({}, air {}): trench water cells {}, ice over the trench {};              wall regolith {regolith}; freeze-thaw wall cells {flipped}",
            row.key,
            row.air_temp,
            t.count("water", trench),
            t.count("ice", trench_cols),
        );
        if row.air_temp < 0 {
            froze = t.count("ice", trench_cols) > 0;
        }
    }
    eprintln!("freeze-thaw bits on wall cells over the run: {thawed}");
    assert!(
        froze,
        "the pond over the trench froze in the season below 0 °C"
    );
    assert!(t.count("water", trench) > 0, "the trench holds water");

    // Regolith beside the water against the control at the same depth.
    let mut compared = 0;
    for &c in &walls {
        if !by_water(t, c, water) {
            continue;
        }
        let ctrl = CellPos::new(control.min.0.x, c.0.y, control.min.0.z);
        let (r, rc) = (
            t.layer_u8(c, LayerId::Regolith),
            t.layer_u8(ctrl, LayerId::Regolith),
        );
        assert_eq!(material(t, ctrl), wall_id, "the control is unchanged");
        assert!(
            r > rc,
            "wall cell {c:?} beside the water has regolith {r}, the control at its depth {rc}"
        );
        compared += 1;
    }
    eprintln!("{compared} wall cells beside the water weathered past the control");
    assert!(compared > 0, "some wall cells are beside the trench water");

    // Further seasons until a wall cell has converted. They are dry, so
    // the runoff no longer rises in the sump; the porous lining keeps its
    // water and the trench its pond, less what evaporates.
    t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: None,
    });
    let mut converted = 0;
    for n in 1..=MAX_FURTHER_SEASONS {
        t.advance_season();
        converted = walls
            .iter()
            .filter(|&&c| material(t, c) == weathered)
            .count();
        let max = walls
            .iter()
            .map(|&c| t.layer_u8(c, LayerId::Regolith))
            .max()
            .unwrap_or(0);
        eprintln!(
            "further season {n}: {converted} wall cells converted, max regolith {max}, \
             weathered_cells {}",
            t.ledger().weathered_cells
        );
        if converted > 0 {
            break;
        }
    }
    assert!(
        converted > 0,
        "a wall cell converted within {MAX_FURTHER_SEASONS} further seasons"
    );
    // Every converted wall cell converted in this season, through the
    // weathering pass, and starts its new material with no regolith.
    assert!(
        u64::from(t.ledger().weathered_cells) >= converted as u64,
        "the ledger counts {} conversions, {converted} walls converted",
        t.ledger().weathered_cells
    );
    for &c in walls.iter().filter(|&&c| material(t, c) == weathered) {
        assert_eq!(t.layer_u8(c, LayerId::Regolith), 0, "{c:?} converted fresh");
    }
    for c in cells(control) {
        assert_eq!(material(t, c), wall_id, "the control never converts");
        assert_eq!(
            t.layer_u8(c, LayerId::Regolith),
            0,
            "the control never weathers"
        );
    }
    shoot(t, trench, "freeze_thaw_weathers_cut");

    // A second shot at the end of the next warmest season, when the ice
    // over the slope has had a summer to thaw.
    let warmest = rows
        .iter()
        .enumerate()
        .max_by_key(|(_, r)| r.air_temp)
        .map(|(i, _)| i)
        .expect("season rows");
    loop {
        let row = season(t) as usize % len;
        t.advance_season();
        if row == warmest {
            break;
        }
    }
    eprintln!(
        "after {}: ice over the trench {}, trench water cells {}, converted wall cells {}",
        rows[warmest].key,
        t.count("ice", trench_cols),
        t.count("water", trench),
        walls
            .iter()
            .filter(|&&c| material(t, c) == weathered)
            .count()
    );
    shoot(t, trench, "freeze_thaw_weathers_cut-summer");
}

/// Saves a screenshot of the trench from high above the slope downhill of
/// it, looking up the slope at the cut. The eye is above the ice and water
/// that winter leaves over the slope.
fn shoot(t: &mut TestApp, trench: CellAabb, name: &str) {
    t.steps(3);
    settle(t);
    {
        let (min, max) = (trench.min.0, trench.max.0);
        let centre = Vec3::new(
            (min.x + max.x + 1) as f32 / 2.0,
            (max.y + 1) as f32,
            (min.z + max.z + 1) as f32 / 2.0,
        ) * CELL_SIZE_M;
        let eye = centre + Vec3::new(7.0, 8.0, 0.0);
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
    }
    t.steps(2);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, name, t.frame());
    eprintln!("screenshot: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "{name}: render failure: {:?}",
        t.render_failure()
    );
}
