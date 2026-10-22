//! The gas tint, readout and lantern icon (SAD-P2 §10, F154): each tinted
//! gas set at the camera's own cell tints the whole view and shows in the
//! readout; a flammable gas at its `ignite_min` turns the lantern icon to
//! danger. The test asserts `HudState` and the HUD nodes; the screenshots
//! are for the playtester (rule 21).

use bevy::prelude::*;
use rmc_client::{
    GameCamera, GasTintOverlay, LanternIcon, LanternState, MeshQueue, MeshStats, gas_overlay,
};
use rmc_protocol::ClientMsg;
use rmc_testkit::bots::look_at;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::layers::LayerId;
use rmc_world::materials::Phase;
use rmc_world::shape::Shape;

/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;
/// The player's feet, 8 m from the wall's near face.
const FEET: Vec3 = Vec3::new(17.25, FLOOR_M, 26.25);
/// A cell of the wall the player looks at.
const WALL: CellPos = CellPos::new(34, 104, 36);

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

fn edit_boxes(t: &mut TestApp, edits: &[(EditOp, CellPos, CellPos)]) {
    for (seq, &(op, min, max)) in (1000..).zip(edits) {
        t.send(ClientMsg::Edit(EditCommand {
            id: CommandId {
                issuer: t.player_id(),
                seq,
            },
            issuer: t.player_id(),
            op,
            shape: Shape::Box { min, max },
        }));
    }
    t.steps(3);
}

fn camera_cell(t: &mut TestApp) -> CellPos {
    let world = t.app_mut().world_mut();
    let mut q = world.query_filtered::<&Transform, With<GameCamera>>();
    CellPos::from_meters(q.single(world).expect("one game camera").translation)
}

/// The overlay's color, if it shows.
fn overlay(t: &mut TestApp) -> Option<Color> {
    let world = t.app_mut().world_mut();
    let mut q = world.query_filtered::<(&BackgroundColor, &Visibility), With<GasTintOverlay>>();
    let (bg, v) = q.single(world).expect("one gas tint overlay");
    (*v == Visibility::Inherited).then_some(bg.0)
}

fn lantern_icon(t: &mut TestApp) -> Color {
    let world = t.app_mut().world_mut();
    let mut q = world.query_filtered::<&BackgroundColor, With<LanternIcon>>();
    q.single(world).expect("one lantern icon").0
}

#[test]
#[ignore]
fn gas_tint_shows() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "gas_tint_shows", gas_tint);
}

fn gas_tint(t: &mut TestApp) {
    settle(t);
    let wall = t
        .host_world()
        .materials()
        .iter()
        .find(|d| d.phase == Phase::Solid && d.has_tag("stone") && d.emissive == 0.0)
        .expect("a plain stone")
        .id;
    // The course, high above the terrain: a floor, the space above it
    // cleared, and a wall across the view.
    edit_boxes(
        t,
        &[
            (
                EditOp::Set(wall),
                CellPos::new(20, 100, 4),
                CellPos::new(43, 100, 59),
            ),
            (
                EditOp::Remove,
                CellPos::new(20, 101, 4),
                CellPos::new(43, 124, 59),
            ),
            (
                EditOp::Set(wall),
                CellPos::new(26, 101, 36),
                CellPos::new(42, 112, 36),
            ),
        ],
    );
    t.spawn_player(FEET);
    t.steps(30);
    let face = Vec3::new(
        (WALL.0.x as f32 + 0.5) * CELL_SIZE_M,
        (WALL.0.y as f32 + 0.5) * CELL_SIZE_M,
        (WALL.0.z + 1) as f32 * CELL_SIZE_M,
    );
    look_at(t, face).unwrap_or_else(|e| panic!("aiming: {e}"));
    settle(t);
    let eye = camera_cell(t);
    assert_eq!(
        t.host_world().get_cell(eye),
        Some(rmc_world::materials::MaterialId::AIR)
    );
    let hud = t.hud();
    assert_eq!(
        (hud.gases.len(), hud.gas_tint, hud.lantern),
        (0, None, LanternState::Steady),
        "clean air: {hud:?}"
    );
    assert_eq!(overlay(t), None);
    let clean = t.screenshot();
    let path = e2e::save_screenshot(&clean, "gas_tint_shows-clean", t.frame());
    eprintln!("clean: {}", path.display());

    // Every tinted gas, alone at full concentration on the camera's cell.
    let tables = t.eco_tables().clone();
    let tints = tables.gas_tints();
    assert!(!tints.is_empty(), "the shipped gas tints");
    for tint in &tints {
        let LayerId::Gas(i) = tint.layer else {
            panic!("a gas tint on {:?}", tint.layer);
        };
        let key = tables.gases()[usize::from(i)].key.clone();
        t.set_layer(eye, tint.layer, 255);
        t.steps(2);
        assert_eq!(camera_cell(t), eye, "the camera stays put");
        let want = gas_overlay(&tints, |l| if l == tint.layer { 255 } else { 0 });
        assert!(want.is_some());
        let hud = t.hud();
        assert_eq!(hud.gas_tint, want, "{key}");
        assert_eq!(
            hud.gases
                .iter()
                .map(|g| (g.name.as_str(), g.amount))
                .collect::<Vec<_>>(),
            [(key.as_str(), 255)]
        );
        assert_eq!(overlay(t), want, "the overlay shows {key}");
        let tinted = t.screenshot();
        let path = e2e::save_screenshot(&tinted, &format!("gas_tint_shows-{key}"), t.frame());
        eprintln!("{key}: {}", path.display());
        assert!(
            tinted.as_raw() != clean.as_raw(),
            "{key}: the frames differ"
        );
        t.set_layer(eye, tint.layer, 0);
        t.steps(2);
        assert_eq!(t.hud().gas_tint, None);
        assert_eq!(overlay(t), None);
    }

    // A flammable gas at its ignite_min: no tint, the lantern in danger.
    let (layer, ignite_min) = *tables.flammable_gases().first().expect("a flammable gas");
    t.set_layer(eye, layer, i32::from(ignite_min));
    t.steps(2);
    let hud = t.hud();
    assert_eq!((hud.gas_tint, hud.lantern), (None, LanternState::Danger));
    assert_eq!(lantern_icon(t), LanternState::Danger.color());
    let danger = t.screenshot();
    let path = e2e::save_screenshot(&danger, "gas_tint_shows-lantern", t.frame());
    eprintln!("lantern: {}", path.display());
    assert!(
        danger.as_raw() != clean.as_raw(),
        "the icon changed the frame"
    );
    assert_eq!(t.render_failure(), None);
}
