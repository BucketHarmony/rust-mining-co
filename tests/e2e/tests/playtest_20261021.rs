//! E3b player's walk (2026-10-21): gas drift, venting, emission, ignition
//! and the gas tint, played the way a miner would meet them.
//!
//! 1. Sim-only sweep (no render): a sealed 8 x 4 x 8 coal working, four
//!    seasons of fill, then a shaft of width 1, 2, 4 or 8 to the surface;
//!    how many seasons until the ceiling is below `ignite_min` and below a
//!    tenth of what it was.
//! 2. A real player (1.7 m, eye 1.55 m) stands in a sealed 4-high coal
//!    working and waits season by season: the HUD readout, the lantern icon
//!    and the view. Then the player digs a one-cell shaft up to the surface
//!    with the pickaxe and waits again.
//! 3. A second working with a magma cell in its east wall at the ceiling's
//!    height: the player waits until the firedamp ignites, with the HUD and
//!    a screenshot each season.
//!
//! Screenshots are named `playtest_20261021-<what>-<frame>.png`; notes go
//! to `<out>/playtest_20261021.log`. Nothing here asserts the game is
//! "good", only that it does not crash.

use std::io::Write;

use bevy::prelude::*;
use rmc_client::controller::Player;
use rmc_client::pickaxe::SWING_BUTTON;
use rmc_client::{GameCamera, LanternState, MeshQueue, MeshStats};
use rmc_testkit::{Fixture, SeasonOverride, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::LayerId;
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

const NAME: &str = "playtest_20261021";
const BOUNDS: (i32, i32, i32) = (63, 127, 63);

fn note(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    eprintln!("{msg}");
    let path = e2e::out_dir().join(format!("{NAME}.log"));
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open the log");
    writeln!(f, "{msg}").expect("write the log");
}

fn bounds() -> CellAabb {
    CellAabb::new(
        CellPos::new(0, 0, 0),
        CellPos::new(BOUNDS.0, BOUNDS.1, BOUNDS.2),
    )
}

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

fn shape(b: CellAabb) -> Shape {
    Shape::Box {
        min: b.min,
        max: b.max,
    }
}

fn grown(b: CellAabb, by: i32) -> CellAabb {
    CellAabb::new(
        CellPos::new(b.min.0.x - by, b.min.0.y - by, b.min.0.z - by),
        CellPos::new(b.max.0.x + by, b.max.0.y + by, b.max.0.z + by),
    )
}

/// The flammable gas and the material that emits it.
struct Fuel {
    layer: LayerId,
    index: usize,
    key: String,
    ignite_min: u8,
    coal: MaterialId,
    smoke: LayerId,
}

fn fuel(t: &TestApp) -> Fuel {
    let tables = t.eco_tables().clone();
    let (layer, ignite_min) = *tables.flammable_gases().first().expect("a flammable gas");
    let LayerId::Gas(index) = layer else {
        panic!("a flammable gas on {layer:?}");
    };
    let key = tables.gases()[usize::from(index)].key.clone();
    let coal = t
        .host_world()
        .materials()
        .iter()
        .find(|d| d.emits.iter().any(|(k, a)| *k == key && *a > 0))
        .expect("a material that emits the gas")
        .id;
    let smoke = tables
        .gas_tints()
        .iter()
        .map(|g| g.layer)
        .find(|l| {
            let LayerId::Gas(i) = *l else { return false };
            tables.gases()[usize::from(i)].key == "smoke"
        })
        .expect("a smoke tint");
    Fuel {
        layer,
        index: usize::from(index),
        key,
        ignite_min,
        coal,
        smoke,
    }
}

/// Mean times ten, and the maximum, of `layer` over the top row `y` of
/// the cave.
fn row_stats(t: &TestApp, layer: LayerId, cave: CellAabb, y: i32) -> (u64, u8) {
    let (mut sum, mut n, mut max) = (0u64, 0u64, 0u8);
    for z in cave.min.0.z..=cave.max.0.z {
        for x in cave.min.0.x..=cave.max.0.x {
            let v = t.layer_u8(CellPos::new(x, y, z), layer);
            sum += u64::from(v);
            n += 1;
            max = max.max(v);
        }
    }
    (sum * 10 / n, max)
}

fn rows(t: &TestApp, layer: LayerId, cave: CellAabb) -> Vec<u64> {
    (cave.min.0.y..=cave.max.0.y)
        .map(|y| {
            t.sum_layer(
                layer,
                CellAabb::new(
                    CellPos::new(cave.min.0.x, y, cave.min.0.z),
                    CellPos::new(cave.max.0.x, y, cave.max.0.z),
                ),
            )
        })
        .collect()
}

fn camera_cell(t: &mut TestApp) -> CellPos {
    let world = t.app_mut().world_mut();
    let mut q = world.query_filtered::<&Transform, With<GameCamera>>();
    CellPos::from_meters(q.single(world).expect("one game camera").translation)
}

fn feet_cell(t: &TestApp) -> CellPos {
    CellPos::from_meters(t.player().pos + Vec3::Y * 0.01)
}

fn shot(t: &mut TestApp, what: &str) {
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("{NAME}-{what}"), t.frame());
    note(format!(
        "[frame {}] shot {what}: {}",
        t.frame(),
        path.display()
    ));
}

/// Sets where the player looks.
fn face(t: &mut TestApp, yaw: f32, pitch: f32) {
    let world = t.app_mut().world_mut();
    let mut q = world.query::<&mut Player>();
    let mut p = q.single_mut(world).expect("one player");
    p.yaw = yaw;
    p.pitch = pitch;
}

/// Moves the (standing) player to `feet`.
fn teleport(t: &mut TestApp, feet: Vec3) {
    let world = t.app_mut().world_mut();
    let mut q = world.query::<&mut Player>();
    let mut p = q.single_mut(world).expect("one player");
    p.feet = feet;
    p.velocity = Vec3::ZERO;
}

fn standing_at(cave: CellAabb) -> Vec3 {
    // The middle of a floor cell near the cave's middle.
    Vec3::new(
        (cave.min.0.x + 4) as f32 * CELL_SIZE_M + CELL_SIZE_M / 2.0,
        cave.min.0.y as f32 * CELL_SIZE_M,
        (cave.min.0.z + 4) as f32 * CELL_SIZE_M + CELL_SIZE_M / 2.0,
    )
}

fn lantern_name(l: LanternState) -> &'static str {
    match l {
        LanternState::Steady => "steady",
        LanternState::Flaring => "FLARING",
        LanternState::Danger => "DANGER",
    }
}

/// One line of what the player reads off the HUD, and the gas around.
fn observe(t: &mut TestApp, f: &Fuel, cave: CellAabb, what: &str) -> LanternState {
    let eye = camera_cell(t);
    let feet = feet_cell(t);
    let top = cave.max.0.y;
    let (mean10, max) = row_stats(t, f.layer, cave, top);
    let hud = t.hud();
    note(format!(
        "[{what}] eye {:?} {} {} {} | feet {} | ceiling mean {}.{} max {} | rows {:?} | HUD gases {:?}, tint {}, lantern {}, temp {:?}",
        eye.0,
        f.key,
        t.layer_u8(eye, f.layer),
        if t.host_world().get_cell(eye) == Some(MaterialId::AIR) {
            "(air)"
        } else {
            "(NOT AIR)"
        },
        t.layer_u8(feet, f.layer),
        mean10 / 10,
        mean10 % 10,
        max,
        rows(t, f.layer, cave),
        hud.gases
            .iter()
            .map(|g| format!("{} {}", g.name, g.amount))
            .collect::<Vec<_>>(),
        hud.gas_tint.is_some(),
        lantern_name(hud.lantern),
        hud.temperature,
    ));
    hud.lantern
}

fn dry(t: &mut TestApp) {
    t.set_season_params(SeasonOverride {
        rain: Some(0),
        air_temp: None,
    });
}

#[test]
#[ignore]
fn playtest_20261021() {
    let _ = std::fs::remove_file(e2e::out_dir().join(format!("{NAME}.log")));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds(),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, NAME, play);
}

fn play(t: &mut TestApp) {
    let stage = std::env::var("PT_STAGE").unwrap_or_default();
    if stage == "study" {
        study(t.config().world_bounds);
        return;
    }
    if stage == "late" {
        late_ignition(t.config().world_bounds);
        return;
    }
    if stage != "render" {
        sweep(t.config().world_bounds);
        study(t.config().world_bounds);
        late_ignition(t.config().world_bounds);
    }
    settle(t);
    let f = fuel(t);
    note(format!(
        "gas {} (layer {:?}), ignite_min {}, coal {:?}",
        f.key, f.layer, f.ignite_min, f.coal
    ));
    sealed_and_dig(t, &f);
    ignition(t, &f);
    assert_eq!(t.render_failure(), None);
}

// ---------------------------------------------------------------------
// 1. How fast does a shaft of each width vent a working?
// ---------------------------------------------------------------------

fn sweep(bounds: CellAabb) {
    for width in [1, 2, 4, 8] {
        let mut t = TestApp::new(TestConfig {
            world_bounds: bounds,
            render: false,
            ..TestConfig::default()
        });
        t.step();
        let f = fuel(&t);
        let site = t.fixture(Fixture::SealedCave);
        let cave = site.feature;
        t.apply_edits(&[
            (EditOp::Set(f.coal), shape(grown(cave, 1))),
            (EditOp::Remove, shape(cave)),
        ]);
        dry(&mut t);
        t.seasons(4);
        let top = cave.max.0.y;
        let (m0, x0) = row_stats(&t, f.layer, cave, top);
        let x_mid = (cave.min.0.x + cave.max.0.x) / 2;
        let z_mid = (cave.min.0.z + cave.max.0.z) / 2;
        let shaft = CellAabb::new(
            CellPos::new(x_mid, top + 1, z_mid),
            CellPos::new(x_mid + width - 1, site.ground_y, z_mid + width - 1),
        );
        t.apply_edits(&[(EditOp::Remove, shape(shaft))]);
        let mut line = format!(
            "sweep width {width}: after 4 sealed seasons ceiling mean {}.{} max {x0}; then per season (mean, max, cave total):",
            m0 / 10,
            m0 % 10
        );
        let mut safe_at = None;
        let mut tenth_at = None;
        for season in 1..=10u32 {
            t.advance_season();
            let (m, x) = row_stats(&t, f.layer, cave, top);
            let total = t.sum_layer(f.layer, cave);
            line += &format!(" s{season}=({}.{}, {x}, {total})", m / 10, m % 10);
            if safe_at.is_none() && x < f.ignite_min {
                safe_at = Some(season);
            }
            if tenth_at.is_none() && m * 10 < m0 {
                tenth_at = Some(season);
            }
        }
        note(format!(
            "{line}; ceiling max below ignite_min after {safe_at:?} seasons, mean under a tenth after {tenth_at:?}"
        ));
    }
}

// ---------------------------------------------------------------------
// 2. A player in a sealed coal working, then a hand-dug shaft.
// ---------------------------------------------------------------------

fn sealed_and_dig(t: &mut TestApp, f: &Fuel) {
    let site = t.fixture(Fixture::SealedCave);
    let cave = site.feature;
    t.apply_edits(&[
        (EditOp::Set(f.coal), shape(grown(cave, 1))),
        (EditOp::Remove, shape(cave)),
    ]);
    dry(t);
    settle(t);
    note(format!("cave {cave:?}, ground {}", site.ground_y));
    t.spawn_player(standing_at(cave));
    face(t, -std::f32::consts::FRAC_PI_2, -0.1);
    t.steps(40);
    note(format!("player after landing: {:?}", t.player()));
    observe(t, f, cave, "season 0");
    shot(t, "sealed-s0");

    // Wait: shoot every season at the start, then on change.
    let mut last = LanternState::Steady;
    let mut first_flaring = None;
    let mut first_danger = None;
    for season in 1..=8u32 {
        t.advance_season();
        t.steps(6);
        let state = observe(t, f, cave, &format!("sealed season {season}"));
        if state == LanternState::Flaring && first_flaring.is_none() {
            first_flaring = Some(season);
        }
        if state == LanternState::Danger && first_danger.is_none() {
            first_danger = Some(season);
        }
        if season == 1 || season == 3 || state != last {
            shot(t, &format!("sealed-s{season}"));
        }
        last = state;
        if state == LanternState::Danger {
            break;
        }
    }
    note(format!(
        "at the player's eye: first flaring season {first_flaring:?}, first danger season {first_danger:?}"
    ));

    // Look up and dig a one-cell shaft with the pickaxe.
    face(t, 0.0, 1.5);
    t.steps(4);
    shot(t, "looking-up-at-roof");
    let player_column = CellPos::from_meters(t.player().pos);
    note(format!(
        "player column {:?}; target {:?}",
        player_column.0,
        t.hud().target
    ));
    let col = (player_column.0.x, player_column.0.z);
    let mut dug_frames = Vec::new();
    for y in (cave.max.0.y + 1)..=site.ground_y {
        let cell = CellPos::new(col.0, y, col.1);
        let material = t.host_world().get_cell(cell);
        t.mouse_down(SWING_BUTTON);
        let mut frames = 0;
        while t.host_world().get_cell(cell) != Some(MaterialId::AIR) && frames < 240 {
            t.step();
            frames += 1;
        }
        t.mouse_up(SWING_BUTTON);
        dug_frames.push((y, material, frames));
        t.steps(2);
        note(format!(
            "hand dig y {y}: {material:?} took {frames} frames; inventory {:?}; target {:?}",
            t.inventory(),
            t.hud().target
        ));
    }
    settle(t);
    let open = (cave.max.0.y + 1..=site.ground_y)
        .all(|y| t.host_world().get_cell(CellPos::new(col.0, y, col.1)) == Some(MaterialId::AIR));
    note(format!("shaft open to the sky: {open}"));
    observe(t, f, cave, "shaft just dug");
    shot(t, "shaft-dug-looking-up");

    // Season by season with the one-cell shaft.
    face(t, -std::f32::consts::FRAC_PI_2, -0.1);
    let mut cleared = None;
    for season in 1..=6u32 {
        t.advance_season();
        t.steps(6);
        let state = observe(t, f, cave, &format!("shaft season {season}"));
        if state != LanternState::Danger && cleared.is_none() {
            cleared = Some(season);
        }
        if season == 1 || season == 6 {
            shot(t, &format!("shaft-s{season}"));
        }
    }
    note(format!(
        "one-cell shaft, player's lantern not in danger after {cleared:?} seasons"
    ));
}

// ---------------------------------------------------------------------
// 3. Firedamp next to magma.
// ---------------------------------------------------------------------

fn ignition(t: &mut TestApp, f: &Fuel) {
    // A coal block under the surface at the east edge, a working cut in
    // it, and a small magma chamber (a 3 x 3 x 3 of magma around a
    // magma_source core) in its east wall, at the ceiling's height. A lone
    // magma cell runs out into the cave and freezes; see the study.
    let cave = CellAabb::new(CellPos::new(52, 36, 4), CellPos::new(59, 39, 11));
    let block = grown(cave, 3);
    let chamber = CellAabb::new(CellPos::new(60, 38, 6), CellPos::new(62, 40, 8));
    let probe = CellPos::new(60, 39, 7);
    let magma = t
        .eco_tables()
        .role_material("magma")
        .expect("a magma material");
    let source = t
        .eco_tables()
        .role_material("magma_source")
        .expect("a magma_source material");
    t.apply_edits(&[
        (EditOp::Set(f.coal), shape(block)),
        (EditOp::Remove, shape(cave)),
        (EditOp::Set(magma), shape(chamber)),
        (EditOp::Set(source), Shape::Cell(CellPos::new(61, 39, 7))),
    ]);
    dry(t);
    teleport(t, standing_at(cave));
    face(t, -std::f32::consts::FRAC_PI_2, 0.0);
    t.steps(20);
    settle(t);
    let blast_zone = grown(cave, 5);
    note(format!(
        "ignition working {cave:?}, chamber {chamber:?}; player {:?}; air in the zone {}, magma {}, obsidian {}, water {}",
        t.player().pos,
        t.count("air", blast_zone),
        t.count("magma", blast_zone),
        t.count("obsidian", blast_zone),
        t.count("water", blast_zone),
    ));
    observe(t, f, cave, "ignition season 0");
    shot(t, "ignite-s0-facing-chamber");

    let mut ignited_in = None;
    for season in 1..=8u32 {
        let air_before = t.count("air", blast_zone);
        t.advance_season();
        t.steps(6);
        let ledger = t.ledger().clone();
        observe(t, f, cave, &format!("ignition season {season}"));
        let eye = camera_cell(t);
        note(format!(
            "  ledger: ignitions {}, react_out[{}] {}, react_in {:?}, emit_in {}, vent_out {}; zone air {} -> {}, magma {}, basalt {}; probe cell {:?}; smoke at eye {}, smoke total {}",
            ledger.ignitions,
            f.key,
            ledger.react_out[f.index],
            ledger.react_in,
            ledger.emit_in[f.index],
            ledger.vent_out[f.index],
            air_before,
            t.count("air", blast_zone),
            t.count("magma", blast_zone),
            t.count("basalt", blast_zone),
            key_of(t, t.host_world().get_cell(probe)),
            t.layer_u8(eye, f.smoke),
            t.sum_layer(f.smoke, blast_zone),
        ));
        let boom = ledger.ignitions > 0;
        if boom || ignited_in.is_some() || season == 1 || season == 3 {
            shot(t, &format!("ignite-s{season}"));
        }
        if boom && ignited_in.is_none() {
            ignited_in = Some(season);
        }
        if let Some(first) = ignited_in
            && season >= first + 2
        {
            break;
        }
    }
    note(format!("first ignition in season {ignited_in:?}"));
    // The view after the dust: idle a while, then once more.
    t.steps(60);
    shot(t, "ignite-after-60-frames");
    // Turn round and look at the blast's west side, and at the floor.
    face(t, std::f32::consts::FRAC_PI_2, -0.5);
    t.steps(4);
    shot(t, "ignite-looking-west-and-down");
}

// ---------------------------------------------------------------------
// 4. Which heat source ignites, and what does it do to the working?
// ---------------------------------------------------------------------

fn key_of(t: &TestApp, m: Option<MaterialId>) -> String {
    m.and_then(|m| t.host_world().materials().iter().find(|d| d.id == m))
        .map_or_else(|| "?".to_string(), |d| d.key.clone())
}

fn study(bounds: CellAabb) {
    for variant in [
        "single magma",
        "single magma_source",
        "3x3x3 magma, source core",
    ] {
        let mut t = TestApp::new(TestConfig {
            world_bounds: bounds,
            render: false,
            ..TestConfig::default()
        });
        t.step();
        let f = fuel(&t);
        let magma = t.eco_tables().role_material("magma").expect("magma");
        let source = t
            .eco_tables()
            .role_material("magma_source")
            .expect("magma_source");
        let cave = CellAabb::new(CellPos::new(52, 36, 4), CellPos::new(59, 39, 11));
        let mut edits = vec![
            (EditOp::Set(f.coal), shape(grown(cave, 3))),
            (EditOp::Remove, shape(cave)),
        ];
        let cells: Vec<CellPos> = match variant {
            "single magma" => {
                edits.push((EditOp::Set(magma), Shape::Cell(CellPos::new(60, 39, 7))));
                vec![CellPos::new(60, 39, 7)]
            }
            "single magma_source" => {
                edits.push((EditOp::Set(source), Shape::Cell(CellPos::new(60, 39, 7))));
                vec![CellPos::new(60, 39, 7)]
            }
            _ => {
                let chamber = CellAabb::new(CellPos::new(60, 38, 6), CellPos::new(62, 40, 8));
                edits.push((EditOp::Set(magma), shape(chamber)));
                edits.push((EditOp::Set(source), Shape::Cell(CellPos::new(61, 39, 7))));
                vec![CellPos::new(60, 39, 7), CellPos::new(61, 39, 7)]
            }
        };
        t.apply_edits(&edits);
        dry(&mut t);
        let zone = grown(cave, 5);
        let mut first = None;
        for season in 1..=14u32 {
            let air0 = t.count("air", zone);
            t.advance_season();
            let ledger = t.ledger().clone();
            let (mean10, max) = row_stats(&t, f.layer, cave, cave.max.0.y);
            let kinds: Vec<String> = cells
                .iter()
                .map(|&c| {
                    format!(
                        "{}@{}C",
                        key_of(&t, t.host_world().get_cell(c)),
                        t.temp(c) / 10
                    )
                })
                .collect();
            note(format!(
                "study [{variant}] season {season}: cells {kinds:?}; ceiling mean {}.{} max {max}; ignitions {}, zone air {air0} -> {}, magma {}, obsidian {}, basalt {}, smoke total {}, firedamp total {}",
                mean10 / 10,
                mean10 % 10,
                ledger.ignitions,
                t.count("air", zone),
                t.count("magma", zone),
                t.count("obsidian", zone),
                t.count("basalt", zone),
                t.sum_layer(f.smoke, zone),
                t.sum_layer(f.layer, zone),
            ));
            if ledger.ignitions > 0 && first.is_none() {
                first = Some(season);
            }
            if first.is_some_and(|s| season >= s + 2) {
                break;
            }
        }
        note(format!("study [{variant}]: first ignition {first:?}"));
    }
}

// ---------------------------------------------------------------------
// 5. A pocket that filled first, then meets a heat source: the big one.
// ---------------------------------------------------------------------

fn late_ignition(bounds: CellAabb) {
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        render: false,
        ..TestConfig::default()
    });
    t.step();
    let f = fuel(&t);
    let source = t
        .eco_tables()
        .role_material("magma_source")
        .expect("magma_source");
    let cave = CellAabb::new(CellPos::new(52, 36, 4), CellPos::new(59, 39, 11));
    t.apply_edits(&[
        (EditOp::Set(f.coal), shape(grown(cave, 3))),
        (EditOp::Remove, shape(cave)),
    ]);
    dry(&mut t);
    t.seasons(12);
    let zone = grown(cave, 8);
    let (mean10, max) = row_stats(&t, f.layer, cave, cave.max.0.y);
    note(format!(
        "late: after 12 sealed seasons ceiling mean {}.{} max {max}; zone air {}, firedamp total {}",
        mean10 / 10,
        mean10 % 10,
        t.count("air", zone),
        t.sum_layer(f.layer, zone)
    ));
    t.apply_edits(&[(EditOp::Set(source), Shape::Cell(CellPos::new(60, 39, 7)))]);
    for season in 1..=3u32 {
        let air0 = t.count("air", zone);
        let gas0 = t.sum_layer(f.layer, zone);
        t.advance_season();
        let ledger = t.ledger().clone();
        note(format!(
            "late: season {season} after the heat source: ignitions {}, zone air {air0} -> {}, firedamp {gas0} -> {}, smoke total {}, react_out {}",
            ledger.ignitions,
            t.count("air", zone),
            t.sum_layer(f.layer, zone),
            t.sum_layer(f.smoke, zone),
            ledger.react_out[f.index],
        ));
    }
}
