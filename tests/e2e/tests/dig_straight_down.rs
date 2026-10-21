//! Digging straight down (owner change F094, SAD §11.4, §11.5): the player
//! stands on a stone floor, looks straight down and swings the pickaxe.
//! The box is narrower than a cell, so each dug cell drops the feet into
//! the one-cell shaft, onto the next cell, which the pickaxe then breaks.
//! After four cells the feet are four cells lower, and on no frame did
//! the box overlap a blocking cell. Then a one-cell-wide stair is cut from
//! the shaft's bottom back to the surface: two one-cell rises the player
//! climbs by walking (step-up), and a two-cell rise it needs a jump for.

use bevy::prelude::*;
use rmc_client::{ControllerConfig, MeshQueue, MeshStats, PLAYER_WIDTH_M, Replica, SWING_BUTTON};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

/// Top cell row of the stone floor; its top face is the surface.
const SURFACE_ROW: i32 = 100;
/// The surface, in meters.
const FLOOR_M: f32 = (SURFACE_ROW + 1) as f32 * CELL_SIZE_M;
/// The shaft's cell column, under the feet.
const SHAFT_X: i32 = 34;
const SHAFT_Z: i32 = 40;
/// The player's feet: on the surface in the middle of the shaft's column.
const FEET: Vec3 = Vec3::new(
    (SHAFT_X as f32 + 0.5) * CELL_SIZE_M,
    FLOOR_M,
    (SHAFT_Z as f32 + 0.5) * CELL_SIZE_M,
);
/// Cells dug straight down.
const DEPTH: i32 = 4;
/// The feet must end at least this many cells below the surface.
const MIN_DROP_CELLS: i32 = 3;
// The box fits one cell column.
const _: () = assert!(PLAYER_WIDTH_M < CELL_SIZE_M);

/// Frames allowed to break one cell and land on the next.
const MAX_FRAMES: u32 = 600;

/// Steps until meshing is idle and nothing waits to upload. Once the
/// player is spawned (`player`), each step is checked like [`step_free`].
fn settle(t: &mut TestApp, player: bool) {
    for _ in 0..240 {
        if player {
            step_free(t);
        } else {
            t.step();
        }
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

/// Sends the edits, then steps 3 times (checked like [`settle`]).
fn edit_boxes(t: &mut TestApp, edits: &[(EditOp, CellPos, CellPos)], player: bool) {
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
    for _ in 0..3 {
        if player {
            step_free(t);
        } else {
            t.step();
        }
    }
}

fn replica_hash(t: &TestApp) -> u64 {
    t.app().world().resource::<Replica>().world().hash()
}

/// One step, then a check that the box overlaps no blocking cell.
fn step_free(t: &mut TestApp) {
    t.step();
    let hits = t.player_penetration();
    assert!(
        hits.is_empty(),
        "frame {}: the box at {:?} overlaps {hits:?}",
        t.frame(),
        t.player().pos
    );
}

/// `n` steps, each checked like [`step_free`].
fn steps_free(t: &mut TestApp, n: u32) {
    for _ in 0..n {
        step_free(t);
    }
}

/// Steps (checking every frame) until `done`, at most [`MAX_FRAMES`] times.
fn step_until(t: &mut TestApp, what: &str, done: impl Fn(&TestApp) -> bool) {
    for _ in 0..MAX_FRAMES {
        if done(t) {
            return;
        }
        step_free(t);
    }
    panic!("{what}: not within {MAX_FRAMES} frames: {:?}", t.player());
}

/// Whether the feet stand on the top face of cell row `row`.
fn standing_on(t: &TestApp, row: i32) -> bool {
    let p = t.player();
    p.grounded && (p.pos.y - (row + 1) as f32 * CELL_SIZE_M).abs() < 0.01
}

#[test]
#[ignore]
fn dig_straight_down() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        size: (320, 180),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "dig_straight_down", play);
}

fn play(t: &mut TestApp) {
    settle(t, false);
    let stone = t
        .host_world()
        .materials()
        .id("stone")
        .expect("stone in the table");

    // The course, high above the terrain: a stone block whose top is the
    // surface, deep enough to dig into, with everything above it cleared.
    edit_boxes(
        t,
        &[
            (
                EditOp::Remove,
                CellPos::new(20, SURFACE_ROW + 1, 4),
                CellPos::new(43, 124, 59),
            ),
            (
                EditOp::Set(stone),
                CellPos::new(20, SURFACE_ROW - 10, 4),
                CellPos::new(43, SURFACE_ROW, 59),
            ),
        ],
        false,
    );
    t.spawn_player(FEET);
    steps_free(t, 30);
    assert!(
        standing_on(t, SURFACE_ROW),
        "on the surface: {:?}",
        t.player()
    );
    settle(t, true);

    // Look straight down (the pitch clamps just short of vertical).
    let sensitivity = t
        .app()
        .world()
        .resource::<ControllerConfig>()
        .sensitivity_rad;
    t.look(0.0, 90f32.to_radians() / sensitivity);
    step_free(t);
    assert!(t.player().pitch < -1.5, "looking down: {:?}", t.player());

    // Dig: hold the swing. Each cell under the feet breaks, the feet drop
    // onto the next one, and the pickaxe targets that one.
    let start = t.player().pos;
    t.mouse_down(SWING_BUTTON);
    for k in 0..DEPTH {
        let row = SURFACE_ROW - k;
        let cell = CellPos::new(SHAFT_X, row, SHAFT_Z);
        step_until(t, "target the cell under the feet", |t| {
            t.hud().target == Some(cell)
        });
        step_until(t, "break it and land on the next", |t| {
            t.host_world().get_cell(cell) == Some(MaterialId::AIR) && standing_on(t, row - 1)
        });
        eprintln!("dug {cell:?}; feet {:?}", t.player().pos);
    }
    t.mouse_up(SWING_BUTTON);
    steps_free(t, 2);
    let bottom = t.player();
    let drop_cells = ((start.y - bottom.pos.y) / CELL_SIZE_M).round() as i32;
    eprintln!("feet dropped {drop_cells} cells: {bottom:?}");
    assert!(drop_cells >= MIN_DROP_CELLS, "{start} -> {bottom:?}");
    assert_eq!(drop_cells, DEPTH);
    assert!(
        (bottom.pos.x - FEET.x).abs() < 1e-3 && (bottom.pos.z - FEET.z).abs() < 1e-3,
        "straight down: {bottom:?}"
    );
    for k in 0..DEPTH {
        let cell = CellPos::new(SHAFT_X, SURFACE_ROW - k, SHAFT_Z);
        assert_eq!(
            t.host_world().get_cell(cell),
            Some(MaterialId::AIR),
            "{cell:?}"
        );
    }
    // The shaft is one cell wide: its walls stand.
    for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
        let wall = CellPos::new(SHAFT_X + dx, SURFACE_ROW - 1, SHAFT_Z + dz);
        assert_eq!(t.host_world().get_cell(wall), Some(stone), "{wall:?}");
    }

    // A one-cell-wide stair along -Z out of the shaft: one cell up at
    // z = 39 and again at z = 38, then two cells up to the surface at
    // z = 37. Everything above the steps is open.
    let floor_row = SURFACE_ROW - DEPTH;
    let steps = [(SHAFT_Z - 1, floor_row + 1), (SHAFT_Z - 2, floor_row + 2)];
    let stair: Vec<_> = steps
        .iter()
        .map(|&(z, step_row)| {
            (
                EditOp::Remove,
                CellPos::new(SHAFT_X, step_row + 1, z),
                CellPos::new(SHAFT_X, SURFACE_ROW, z),
            )
        })
        .collect();
    edit_boxes(t, &stair, true);
    let to_surface = SURFACE_ROW - steps[1].1;
    assert_eq!(to_surface, 2, "the last rise needs a jump");
    settle(t, true);
    assert_eq!(replica_hash(t), t.host_world().hash());

    // Level the view and walk forward (-Z): two step-ups.
    t.look(0.0, -90f32.to_radians() / sensitivity);
    step_free(t);
    t.press(KeyCode::KeyW);
    for &(z, step_row) in &steps {
        step_until(t, "climb a one-cell step", |t| {
            standing_on(t, step_row) && t.player().pos.z < (z + 1) as f32 * CELL_SIZE_M
        });
        assert!(t.player().velocity.y <= 0.0, "walked up, no jump");
        eprintln!("on step z = {z}: {:?}", t.player().pos);
    }
    // The two-cell rise stops the walk ...
    let face_m = (steps[1].0) as f32 * CELL_SIZE_M;
    step_until(t, "walk up to the two-cell rise", |t| {
        t.player().pos.z - PLAYER_WIDTH_M / 2.0 <= face_m + 0.01
    });
    steps_free(t, 10);
    assert!(
        standing_on(t, steps[1].1),
        "still on the step: {:?}",
        t.player()
    );
    // ... and a running jump gets onto the surface.
    t.press(KeyCode::Space);
    step_until(t, "jump onto the surface", |t| {
        standing_on(t, SURFACE_ROW) && t.player().pos.z < face_m
    });
    t.release(KeyCode::Space);
    t.release(KeyCode::KeyW);
    steps_free(t, 10);
    let out = t.player();
    eprintln!("out of the shaft: {out:?}");
    assert!(standing_on(t, SURFACE_ROW), "on the surface: {out:?}");
    assert!(
        (out.pos.x - FEET.x).abs() < 1e-3,
        "stayed in the one-cell stair's column: {out:?}"
    );

    // The view: turned back, looking down the stair into the shaft.
    t.look(
        std::f32::consts::PI / sensitivity,
        50f32.to_radians() / sensitivity,
    );
    step_free(t);
    settle(t, true);
    let shot = t.screenshot();
    let path = e2e::save_screenshot(&shot, "dig_straight_down", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None);
}
