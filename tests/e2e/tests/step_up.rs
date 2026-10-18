//! Auto step-up through injected input (SAD §11.4, §12.4): the player walks
//! onto a one-cell ledge without jumping, and a two-cell wall stops them,
//! both on the ledge and on the floor.

use bevy::prelude::*;
use rmc_client::controller::SKIN_M;
use rmc_client::{
    ControllerConfig, GameCamera, MeshQueue, MeshStats, PLAYER_WIDTH_M, STEP_SMOOTH_S,
};
use rmc_protocol::ClientMsg;
use rmc_testkit::{PlayerSnapshot, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::shape::Shape;

/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;
/// Top of the ledge (one cell, layer y = 101).
const LEDGE_M: f32 = 102.0 * CELL_SIZE_M;
/// The ledge's face: it covers cell rows z < 40.
const LEDGE_FACE_M: f32 = 40.0 * CELL_SIZE_M;
/// The wall's face: two cells (layers y = 102, 103) on the ledge, rows z < 32.
const WALL_FACE_M: f32 = 32.0 * CELL_SIZE_M;

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

/// What a walk saw, frame by frame.
#[derive(Default)]
struct Trace {
    /// Frames whose fixed step lifted the feet, with the rise in meters.
    rises: Vec<(u32, f32)>,
    max_feet_y: f32,
    max_up_velocity: f32,
    /// Largest per-frame rise of the camera.
    max_camera_rise: f32,
}

/// Steps `n` frames with whatever keys are held. Every frame the box must
/// be free of solid cells and the player must not be jumping.
fn walk(t: &mut TestApp, n: u32, what: &str, trace: &mut Trace) {
    for _ in 0..n {
        let before = t.player();
        let cam_before = camera(t);
        t.step();
        let frame = t.frame();
        let p = t.player();
        let cells = t.player_penetration();
        assert!(
            cells.is_empty(),
            "{what}, frame {frame}: the player {p:?} overlaps solid cells {cells:?}"
        );
        if p.pos.y > before.pos.y + 1e-4 {
            trace.rises.push((frame, p.pos.y - before.pos.y));
        }
        trace.max_feet_y = trace.max_feet_y.max(p.pos.y);
        trace.max_up_velocity = trace.max_up_velocity.max(p.velocity.y);
        trace.max_camera_rise = trace.max_camera_rise.max(camera(t).y - cam_before.y);
    }
}

/// Walks until `done` holds, at most `max` frames. Returns the frames
/// walked.
fn walk_until(
    t: &mut TestApp,
    max: u32,
    what: &str,
    trace: &mut Trace,
    done: impl Fn(&PlayerSnapshot) -> bool,
) -> u32 {
    let mut frames = 0;
    while !done(&t.player()) {
        assert!(frames < max, "{what} within {max} frames: {:?}", t.player());
        walk(t, 1, what, trace);
        frames += 1;
    }
    frames
}

#[test]
#[ignore]
fn step_up() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "step_up", course);
}

fn course(t: &mut TestApp) {
    settle(t);

    // The course, high above the terrain, everything above the floor
    // cleared first. A stone floor with two lanes, both facing -Z:
    // - lane A (x 15 to 22 m): a one-cell ledge from z = 20 m back, and a
    //   two-cell wall standing on the ledge from z = 16 m back;
    // - lane B (x 10 to 14 m): a two-cell wall standing on the floor from
    //   z = 20 m back.
    let stone = plain_solid(t);
    edit_boxes(
        t,
        &[
            (
                EditOp::Remove,
                CellPos::new(20, 101, 4),
                CellPos::new(43, 124, 59),
            ),
            (
                EditOp::Set(stone),
                CellPos::new(20, 100, 4),
                CellPos::new(43, 100, 59),
            ),
            (
                EditOp::Set(stone),
                CellPos::new(30, 101, 4),
                CellPos::new(43, 101, 39),
            ),
            (
                EditOp::Set(stone),
                CellPos::new(30, 102, 4),
                CellPos::new(43, 103, 31),
            ),
            (
                EditOp::Set(stone),
                CellPos::new(20, 101, 4),
                CellPos::new(27, 102, 39),
            ),
        ],
    );
    settle(t);
    let host = t.host_world();
    let is_stone = |x, y, z| host.get_cell(CellPos::new(x, y, z)) == Some(stone);
    assert!(is_stone(34, 101, 39), "ledge");
    assert!(is_stone(34, 103, 31), "wall on the ledge");
    assert!(is_stone(24, 102, 39), "wall on the floor");
    assert!(!is_stone(34, 101, 40) && !is_stone(34, 102, 32) && !is_stone(34, 104, 31));
    assert!(!is_stone(24, 101, 40) && !is_stone(24, 103, 39));
    assert!(
        !is_stone(28, 101, 39) && !is_stone(29, 101, 39),
        "a gap between the lanes"
    );

    // Lane A, on the floor 2.7 m short of the ledge, facing it (-Z).
    t.spawn_player(Vec3::new(17.0, FLOOR_M + 0.25, 23.0));
    t.step();
    let mut trace = Trace::default();
    walk(t, 30, "landing", &mut trace);
    let start = t.player();
    assert!(
        start.grounded && (start.pos.y - FLOOR_M).abs() < 0.01,
        "{start:?}"
    );

    // Holding W (never Space) walks onto the ledge: the feet rise exactly
    // one cell, once, with no upward velocity, and never above the ledge.
    let mut trace = Trace::default();
    t.press(KeyCode::KeyW);
    let frames = walk_until(t, 120, "walking onto the ledge", &mut trace, |p| {
        p.pos.z <= LEDGE_FACE_M - PLAYER_WIDTH_M
    });
    let on_ledge = t.player();
    eprintln!(
        "on the ledge after {frames} frames: {on_ledge:?}; rises {:?}",
        trace.rises
    );
    assert!(on_ledge.grounded, "standing on the ledge: {on_ledge:?}");
    assert!((on_ledge.pos.y - LEDGE_M).abs() < 0.01, "{on_ledge:?}");
    assert_eq!(trace.rises.len(), 1, "one rise: {:?}", trace.rises);
    assert!(
        (trace.rises[0].1 - CELL_SIZE_M).abs() < 0.01,
        "the rise is one cell: {:?}",
        trace.rises
    );
    assert!(
        trace.max_up_velocity <= 0.0,
        "no jump: {}",
        trace.max_up_velocity
    );
    assert!(
        trace.max_feet_y <= LEDGE_M + 0.01,
        "no hop: {}",
        trace.max_feet_y
    );
    // The feet snap up; the camera follows over STEP_SMOOTH_S.
    let smooth_per_frame = CELL_SIZE_M / STEP_SMOOTH_S / 60.0;
    assert!(
        trace.max_camera_rise <= smooth_per_frame + 1e-3,
        "the camera rises at most {smooth_per_frame} m per frame, rose {}",
        trace.max_camera_rise
    );

    // Still holding W, the player walks on to the wall on the ledge (two
    // cells above it) and stops against it, still on the ledge.
    walk(t, 90, "walking to the wall on the ledge", &mut trace);
    let at_wall = t.player();
    eprintln!("at the wall on the ledge: {at_wall:?}");
    let stop_z = WALL_FACE_M + SKIN_M + PLAYER_WIDTH_M / 2.0;
    assert!(
        (at_wall.pos.z - stop_z).abs() < 1e-3,
        "the wall stops the player at z = {stop_z}: {at_wall:?}"
    );
    assert!(
        at_wall.grounded && (at_wall.pos.y - LEDGE_M).abs() < 0.01,
        "{at_wall:?}"
    );

    // Pushing on, sprinting too, does not climb it.
    walk(t, 30, "pushing on the wall", &mut trace);
    t.press(KeyCode::ShiftLeft);
    walk(t, 30, "sprinting into the wall", &mut trace);
    t.release(KeyCode::ShiftLeft);
    t.release(KeyCode::KeyW);
    walk(t, 2, "stopping", &mut trace);
    let pushed = t.player();
    assert!((pushed.pos.z - stop_z).abs() < 1e-3, "{pushed:?}");
    assert!((pushed.pos.y - LEDGE_M).abs() < 0.01, "{pushed:?}");
    assert_eq!(trace.rises.len(), 1, "no second rise: {:?}", trace.rises);
    assert!(trace.max_up_velocity <= 0.0);
    assert!(trace.max_feet_y <= LEDGE_M + 0.01);

    // Back off the ledge (S), onto the floor, and across to lane B (A).
    t.press(KeyCode::KeyS);
    walk_until(t, 150, "backing off the ledge", &mut trace, |p| {
        p.pos.z >= 23.0
    });
    t.release(KeyCode::KeyS);
    walk(t, 30, "landing back on the floor", &mut trace);
    let back = t.player();
    assert!(
        back.grounded && (back.pos.y - FLOOR_M).abs() < 0.01,
        "back on the floor: {back:?}"
    );
    t.press(KeyCode::KeyA);
    walk_until(t, 120, "strafing to lane B", &mut trace, |p| {
        p.pos.x <= 12.0
    });
    t.release(KeyCode::KeyA);
    walk(t, 2, "stopping", &mut trace);

    // Lane B: W walks into the two-cell wall standing on the floor, and it
    // stops the player on the floor, walking and sprinting.
    let mut trace = Trace::default();
    t.press(KeyCode::KeyW);
    walk(t, 90, "walking to the wall on the floor", &mut trace);
    t.press(KeyCode::ShiftLeft);
    walk(t, 30, "sprinting into the wall on the floor", &mut trace);
    t.release(KeyCode::ShiftLeft);
    t.release(KeyCode::KeyW);
    walk(t, 2, "stopping", &mut trace);
    let blocked = t.player();
    eprintln!("at the wall on the floor: {blocked:?}");
    let stop_z = LEDGE_FACE_M + SKIN_M + PLAYER_WIDTH_M / 2.0;
    assert!(
        (blocked.pos.z - stop_z).abs() < 1e-3,
        "the two-cell wall stops the player at z = {stop_z}: {blocked:?}"
    );
    assert!(
        blocked.grounded && (blocked.pos.y - FLOOR_M).abs() < 0.01,
        "{blocked:?}"
    );
    assert!(trace.rises.is_empty(), "no climb: {:?}", trace.rises);
    assert!(trace.max_up_velocity <= 0.0);

    // Record the view: back up, turn a little right and look down, so both
    // lanes show: the wall on the floor ahead, the ledge with its wall to
    // the right.
    t.press(KeyCode::KeyS);
    walk(t, 30, "backing up for the view", &mut trace);
    t.release(KeyCode::KeyS);
    let sensitivity = t
        .app()
        .world()
        .resource::<ControllerConfig>()
        .sensitivity_rad;
    t.look(
        25f32.to_radians() / sensitivity,
        25f32.to_radians() / sensitivity,
    );
    walk(t, 2, "turning for the view", &mut trace);
    settle(t);
    assert!(camera(t).distance(t.player().eye) < 1e-3);
    let shot = t.screenshot();
    let path = e2e::save_screenshot(&shot, "step_up", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None);
}
