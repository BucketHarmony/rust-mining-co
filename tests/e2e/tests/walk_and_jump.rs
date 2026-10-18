//! Walking and jumping through injected input (SAD §11.4, §12.4): holding W
//! for 60 frames moves the player at least 2 m forward, a jump leaves the
//! ground and lands, and the player's box never shares volume with a solid
//! cell.

use bevy::prelude::*;
use rmc_client::{ControllerConfig, GameCamera, MeshQueue, MeshStats};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::shape::Shape;

/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;

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
    for (seq, &(op, min, max)) in (1..).zip(edits) {
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

fn plain_solid(t: &TestApp) -> MaterialId {
    t.host_world()
        .materials()
        .iter()
        .find(|d| d.phase == Phase::Solid && d.emissive == 0.0 && d.diggable)
        .expect("assets/materials.ron has a plain diggable solid material")
        .id
}

fn camera(t: &mut TestApp) -> Vec3 {
    let world = t.app_mut().world_mut();
    world
        .query_filtered::<&Transform, With<GameCamera>>()
        .single(world)
        .expect("one game camera")
        .translation
}

/// One frame, then the invariants every frame must hold: the box is free
/// of solid cells, and the camera is on the segment between the eyes of the
/// last two fixed steps (render interpolation, SAD §11.4).
fn step_checked(t: &mut TestApp, what: &str) {
    t.step();
    let frame = t.frame();
    let cells = t.player_penetration();
    assert!(
        cells.is_empty(),
        "{what}, frame {frame}: the player {:?} overlaps solid cells {cells:?}",
        t.player()
    );
    let eye = t.player_render_eye().expect("the player has moved");
    let cam = camera(t);
    let span = eye.prev.distance(eye.curr);
    assert!(
        cam.distance(eye.prev) <= span + 1e-4 && cam.distance(eye.curr) <= span + 1e-4,
        "{what}, frame {frame}: camera {cam:?} is not between eyes {:?} and {:?}",
        eye.prev,
        eye.curr
    );
}

fn steps_checked(t: &mut TestApp, n: u32, what: &str) {
    for _ in 0..n {
        step_checked(t, what);
    }
}

#[test]
#[ignore]
fn walk_and_jump() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "walk_and_jump", course);
}

fn course(t: &mut TestApp) {
    settle(t);

    // The course, high above the terrain: a 12 m × 28 m stone floor with
    // its top at FLOOR_M, and everything above it cleared.
    let stone = plain_solid(t);
    edit_boxes(
        t,
        &[
            (
                EditOp::Set(stone),
                CellPos::new(20, 100, 4),
                CellPos::new(43, 100, 59),
            ),
            (
                EditOp::Remove,
                CellPos::new(20, 101, 4),
                CellPos::new(43, 124, 59),
            ),
        ],
    );
    settle(t);
    assert_eq!(
        t.host_world().get_cell(CellPos::new(32, 100, 52)),
        Some(stone)
    );

    // Spawned half a meter up, the player falls onto the floor.
    t.spawn_player(Vec3::new(16.0, FLOOR_M + 0.5, 26.0));
    t.step();
    steps_checked(t, 30, "falling onto the floor");
    let start = t.player();
    assert!(start.grounded, "landed on the floor: {start:?}");
    assert!((start.pos.y - FLOOR_M).abs() < 0.01, "{start:?}");
    assert_eq!(start.yaw, 0.0, "facing -Z");

    // Holding W for 60 frames walks forward (-Z) at least 2 m.
    let config = t.app().world().resource::<ControllerConfig>().clone();
    t.press(KeyCode::KeyW);
    steps_checked(t, 60, "walking forward");
    t.release(KeyCode::KeyW);
    steps_checked(t, 2, "stopping");
    let walked = t.player();
    let moved = walked.pos - start.pos;
    eprintln!("walked {moved:?} in 60 frames");
    assert!(
        -moved.z >= 2.0,
        "W moves the player ≥ 2 m forward: {moved:?}"
    );
    assert!(
        -moved.z <= config.walk_speed_mps * 61.0 / 60.0,
        "at walking speed: {moved:?}"
    );
    assert!(moved.x.abs() < 1e-4, "straight ahead: {moved:?}");
    assert!(walked.grounded && (walked.pos.y - FLOOR_M).abs() < 0.01);

    // Space jumps: the player leaves the ground, rises about v²/2g, and
    // lands where it took off. Space is released long before landing, so
    // it jumps once.
    t.press(KeyCode::Space);
    steps_checked(t, 3, "taking off");
    t.release(KeyCode::Space);
    let lifted = t.player();
    assert!(!lifted.grounded, "the jump leaves the ground: {lifted:?}");
    assert!(lifted.pos.y > FLOOR_M + 0.1, "{lifted:?}");
    let mut apex = lifted.pos.y;
    let mut air_frames = 3;
    while !t.player().grounded {
        assert!(air_frames < 120, "the jump lands within 2 s");
        step_checked(t, "in the air");
        apex = apex.max(t.player().pos.y);
        air_frames += 1;
    }
    let landed = t.player();
    let height = apex - FLOOR_M;
    let expected = config.jump_speed_mps.powi(2) / (2.0 * config.gravity_mps2);
    eprintln!("jump: {height:.3} m high (expected {expected:.3}), {air_frames} frames in the air");
    assert!(
        (height - expected).abs() <= config.jump_speed_mps / 60.0,
        "jump height {height} m, expected about {expected} m"
    );
    let air_s = 2.0 * config.jump_speed_mps / config.gravity_mps2;
    assert!(
        (air_frames as f32 - air_s * 60.0).abs() <= 3.0,
        "{air_frames} frames in the air, expected about {}",
        air_s * 60.0
    );
    assert!(
        (landed.pos.y - FLOOR_M).abs() < 0.01,
        "lands on the floor: {landed:?}"
    );
    assert!(
        (landed.pos.xz() - walked.pos.xz()).length() < 1e-4,
        "lands where it took off: {landed:?}"
    );
    steps_checked(t, 10, "standing after the jump");
    assert!(t.player().grounded, "a released Space does not jump again");

    // The mouse turns the player a quarter turn right; W then walks +X.
    t.look(std::f32::consts::FRAC_PI_2 / config.sensitivity_rad, 0.0);
    step_checked(t, "turning");
    let turned = t.player();
    assert!(
        (turned.yaw + std::f32::consts::FRAC_PI_2).abs() < 1e-4,
        "{turned:?}"
    );
    t.press(KeyCode::KeyW);
    steps_checked(t, 30, "walking after the turn");
    t.release(KeyCode::KeyW);
    steps_checked(t, 2, "stopping");
    let moved = t.player().pos - turned.pos;
    eprintln!("after the turn: walked {moved:?} in 30 frames");
    assert!(moved.x >= 1.0 && moved.z.abs() < 1e-3, "{moved:?}");

    // Look down at the floor and record the view.
    t.look(0.0, 20f32.to_radians() / config.sensitivity_rad);
    step_checked(t, "looking down");
    settle(t);
    let eye = t.player().eye;
    assert!(camera(t).distance(eye) < 1e-3, "the camera is at the eye");
    let shot = t.screenshot();
    let path = e2e::save_screenshot(&shot, "walk_and_jump", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None);
}
