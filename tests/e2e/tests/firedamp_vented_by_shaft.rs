//! Firedamp vented by a shaft (SAD-P2 §12). A sealed cave is cut into a
//! coal seam: the seam is its floor, and its walls and roof too. For three
//! seasons the coal emits firedamp (§7.4 step 4) and the light gas
//! collects under the roof until the ceiling holds at least `ignite_min`
//! (the HUD's lantern is in danger there). Then a shaft is dug from the
//! cave to the surface, and within one season the ceiling's concentration
//! falls below a tenth of what it was, though the seam keeps emitting.
//!
//! The cave is two cells high and the shaft as wide as the cave; see the
//! F156 decisions in `PROGRESS.md` for why (at the shipped rates a floor
//! seam alone gives at most 48 per column in three seasons, and a narrow
//! shaft vents diffusion-limited, over many seasons).

use bevy::prelude::*;
use rmc_client::{GameCamera, LanternState, MeshQueue, MeshStats};
use rmc_testkit::{Fixture, SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::shape::Shape;

/// Seasons the sealed cave fills (SAD-P2 §12).
const FILL_SEASONS: u32 = 3;
/// Rows of the fixture's cave the seam fills from below, leaving a low
/// working two cells high.
const SEAM_ROWS: i32 = 2;

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
fn firedamp_vented_by_shaft() {
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "firedamp_vented_by_shaft", run);
}

fn shape(b: CellAabb) -> Shape {
    Shape::Box {
        min: b.min,
        max: b.max,
    }
}

fn grown(b: CellAabb) -> CellAabb {
    CellAabb::new(
        CellPos::new(b.min.0.x - 1, b.min.0.y - 1, b.min.0.z - 1),
        CellPos::new(b.max.0.x + 1, b.max.0.y + 1, b.max.0.z + 1),
    )
}

fn volume(b: CellAabb) -> u32 {
    let (min, max) = (b.min.0, b.max.0);
    u32::try_from((max.x - min.x + 1) * (max.y - min.y + 1) * (max.z - min.z + 1)).unwrap()
}

fn run(t: &mut TestApp) {
    settle(t);
    let tables = t.eco_tables().clone();
    let (layer, ignite_min) = *tables.flammable_gases().first().expect("a flammable gas");
    let LayerId::Gas(gas) = layer else {
        panic!("a flammable gas on {layer:?}");
    };
    let key = tables.gases()[usize::from(gas)].key.clone();
    // The seam: a material that emits the flammable gas (coal).
    let seam = t
        .host_world()
        .materials()
        .iter()
        .find(|d| d.emits.iter().any(|(k, amount)| *k == key && *amount > 0))
        .unwrap_or_else(|| panic!("no material emits {key}"))
        .id;

    // The fixture's sealed cave, its shell and its lower rows made seam.
    let site = t.fixture(Fixture::SealedCave);
    let fixture_cave = site.feature;
    let cave = CellAabb::new(
        CellPos::new(
            fixture_cave.min.0.x,
            fixture_cave.min.0.y + SEAM_ROWS,
            fixture_cave.min.0.z,
        ),
        fixture_cave.max,
    );
    let ceiling = CellAabb::new(
        CellPos::new(cave.min.0.x, cave.max.0.y, cave.min.0.z),
        cave.max,
    );
    let shaft = CellAabb::new(
        CellPos::new(cave.min.0.x, cave.max.0.y + 1, cave.min.0.z),
        CellPos::new(cave.max.0.x, site.ground_y, cave.max.0.z),
    );
    eprintln!(
        "cave {cave:?}, ceiling {ceiling:?}, shaft {shaft:?}, ground {}",
        site.ground_y
    );
    t.apply_edits(&[
        (EditOp::Set(seam), shape(grown(fixture_cave))),
        (EditOp::Remove, shape(cave)),
    ]);
    assert_eq!(t.count("air", cave), volume(cave), "the cave is open");
    let shell = grown(cave);
    assert_eq!(
        t.host_world()
            .get_cell(CellPos::new(cave.min.0.x, cave.min.0.y - 1, cave.min.0.z)),
        Some(seam),
        "the cave lies on the seam"
    );
    assert_eq!(
        t.host_world().get_cell(shell.max),
        Some(seam),
        "the seam forms the roof"
    );
    assert_eq!(t.count("air", shaft), 0, "the roof is closed");
    assert_eq!(t.sum_layer(layer, cave), 0, "no gas yet");
    // Dry seasons: rain in the shaft would put water on the seam.
    t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: None,
    });

    // Three sealed seasons: the gas collects under the roof.
    t.seasons(FILL_SEASONS);
    let cells = u64::from(volume(ceiling));
    let before = t.sum_layer(layer, ceiling);
    eprintln!(
        "after {FILL_SEASONS} sealed seasons: ceiling {key} {before} over {cells} cells \
         (mean {}), cave {}, rows {:?}",
        before / cells,
        t.sum_layer(layer, cave),
        rows(t, layer, cave),
    );
    assert!(
        before >= cells * u64::from(ignite_min),
        "the ceiling's {key} averages {} < ignite_min {ignite_min}",
        before / cells
    );
    // The camera in the ceiling cell with the most gas, the first in
    // (z, x) order: the lantern flares there.
    let y = ceiling.max.0.y;
    let eye = (ceiling.min.0.z..=ceiling.max.0.z)
        .flat_map(|z| (ceiling.min.0.x..=ceiling.max.0.x).map(move |x| CellPos::new(x, y, z)))
        .rev()
        .max_by_key(|&c| t.layer_u8(c, layer))
        .expect("a ceiling cell");
    look_from(t, eye, Vec3::new(1.0, -0.15, 0.6));
    eprintln!("eye {eye:?}: {key} {}", t.layer_u8(eye, layer));
    assert!(t.layer_u8(eye, layer) >= ignite_min);
    let hud = t.hud();
    assert_eq!(hud.lantern, LanternState::Danger, "under the roof: {hud:?}");
    shot(t, "sealed");

    // Dig the shaft through the roof to the surface.
    t.apply_edits(&[(EditOp::Remove, shape(shaft))]);
    assert_eq!(t.count("air", shaft), volume(shaft), "the shaft is open");
    let floor = cave.min.0.y - 1;
    for (x, z) in [(cave.min.0.x, cave.min.0.z), (cave.max.0.x, cave.max.0.z)] {
        assert_eq!(
            i32::from(t.host_world().surface_y(x, z)),
            floor,
            "the cave is open to the sky at ({x}, {z})"
        );
    }

    t.advance_season();
    let after = t.sum_layer(layer, ceiling);
    let ledger = t.ledger().clone();
    eprintln!(
        "one season with the shaft: ceiling {key} {before} -> {after} (mean {}), cave {}, rows \
         {:?}; ledger vent_out {}, emit_in {}",
        after / cells,
        t.sum_layer(layer, cave),
        rows(t, layer, cave),
        ledger.vent_out[usize::from(gas)],
        ledger.emit_in[usize::from(gas)],
    );
    assert!(
        ledger.emit_in[usize::from(gas)] > 0,
        "the seam kept emitting"
    );
    assert!(
        ledger.vent_out[usize::from(gas)] >= i64::try_from(before - after).unwrap(),
        "the ceiling's {key} left by venting"
    );
    assert!(
        after * 10 < before,
        "the ceiling kept {after} of {before}, not under a tenth"
    );
    look_from(t, eye, Vec3::new(0.3, 1.0, 0.2));
    let hud = t.hud();
    assert_ne!(
        hud.lantern,
        LanternState::Danger,
        "under the shaft: {hud:?}"
    );
    shot(t, "vented");
}

/// The sum of `layer` in each row of `region`, bottom first.
fn rows(t: &TestApp, layer: LayerId, region: CellAabb) -> Vec<u64> {
    (region.min.0.y..=region.max.0.y)
        .map(|y| {
            t.sum_layer(
                layer,
                CellAabb::new(
                    CellPos::new(region.min.0.x, y, region.min.0.z),
                    CellPos::new(region.max.0.x, y, region.max.0.z),
                ),
            )
        })
        .collect()
}

/// Puts the game camera in the middle of `cell`, looking along `dir`, and
/// steps until the mesh and the HUD have caught up.
fn look_from(t: &mut TestApp, cell: CellPos, dir: Vec3) {
    settle(t);
    let eye = (cell.0.as_vec3() + Vec3::splat(0.5)) * CELL_SIZE_M;
    {
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_translation(eye).looking_to(dir, Vec3::Y);
    }
    t.steps(2);
}

fn shot(t: &mut TestApp, what: &str) {
    let image = t.screenshot();
    let path = e2e::save_screenshot(
        &image,
        &format!("firedamp_vented_by_shaft-{what}"),
        t.frame(),
    );
    eprintln!("{what}: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "render failure: {:?}",
        t.render_failure()
    );
}
