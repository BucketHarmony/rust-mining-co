//! The HUD's spoil indicator (SAD-P2 §10, F145): looking at a wall cell
//! whose substance reaches the player's toxicity limit shows it; clearing
//! the substance hides it. The test asserts `HudState` and the indicator
//! node; the screenshots are for the playtester (rule 21).

use bevy::prelude::*;
use rmc_client::{MeshQueue, MeshStats, SPOIL_LABEL, SpoilIndicator};
use rmc_protocol::ClientMsg;
use rmc_testkit::bots::look_at;
use rmc_testkit::{TestApp, TestConfig, ToxKind, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::Phase;
use rmc_world::shape::Shape;

/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;
/// The wall cell the player looks at.
const TARGET: CellPos = CellPos::new(34, 104, 36);
/// The player's feet, 1.75 m from the wall's near face.
const FEET: Vec3 = Vec3::new(17.25, FLOOR_M, 20.25);

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

/// Whether the indicator node is visible, checking it says [`SPOIL_LABEL`].
fn indicator_shown(t: &mut TestApp) -> bool {
    let world = t.app_mut().world_mut();
    let mut q = world.query_filtered::<(&Visibility, &Text), With<SpoilIndicator>>();
    let (v, text) = q.single(world).expect("one spoil indicator");
    assert_eq!(text.0, SPOIL_LABEL);
    *v == Visibility::Inherited
}

#[test]
#[ignore]
fn spoil_indicator_shows() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "spoil_indicator_shows", spoil);
}

fn spoil(t: &mut TestApp) {
    settle(t);
    let wall = t
        .host_world()
        .materials()
        .iter()
        .find(|d| d.phase == Phase::Solid && d.has_tag("stone") && d.emissive == 0.0)
        .expect("a plain stone")
        .id;
    // The course, high above the terrain: a floor, the space above it
    // cleared, and a wall across the view with TARGET in it.
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
        (TARGET.0.x as f32 + 0.5) * CELL_SIZE_M,
        (TARGET.0.y as f32 + 0.5) * CELL_SIZE_M,
        (TARGET.0.z + 1) as f32 * CELL_SIZE_M,
    );
    look_at(t, face).unwrap_or_else(|e| panic!("aiming: {e}"));
    settle(t);
    assert_eq!(
        t.hud().target,
        Some(TARGET),
        "the crosshair is on the target"
    );
    assert!(!t.hud().spoil, "a clean wall is not spoil");
    assert!(!indicator_shown(t));

    // The player's first limited substance, at its limit, on the target.
    let profile = t.eco_tables().tox_profile(ToxKind::Player);
    let (id, limit) = profile.limits().next().expect("a player limit");
    t.set_layer(TARGET, id, i32::from(limit));
    t.step();
    assert_eq!(
        t.spoil_cells(CellAabb::new(TARGET, TARGET), profile),
        vec![TARGET]
    );
    let hud = t.hud();
    assert_eq!(hud.target, Some(TARGET));
    assert!(hud.spoil, "the target is spoil: {hud:?}");
    assert!(indicator_shown(t), "the indicator shows");
    let shown = t.screenshot();
    let path = e2e::save_screenshot(&shown, "spoil_indicator_shows", t.frame());
    eprintln!("spoil: {}", path.display());

    // Cleared, it hides.
    t.set_layer(TARGET, id, 0);
    t.step();
    assert!(!t.hud().spoil);
    assert!(!indicator_shown(t), "the indicator hides");
    let hidden = t.screenshot();
    let path = e2e::save_screenshot(&hidden, "spoil_indicator_shows-clear", t.frame());
    eprintln!("clear: {}", path.display());
    assert!(shown.as_raw() != hidden.as_raw(), "the frames differ");
    assert_eq!(t.render_failure(), None);
}
