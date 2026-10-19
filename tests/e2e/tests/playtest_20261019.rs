//! M7 player's walk (2026-10-19) through the default generated world: drop
//! in at the spawn, look in four directions and up and down (horizon, fog,
//! sky), walk, hold the swing on topsoil and on the hardest diggable solid
//! while logging the HUD's swing pulse and break effect each frame, dig a
//! forward tunnel by hand, stand against a placed wall (lantern falloff),
//! and place a cell on top of the player's own body (overlap safety).
//! Screenshots every 60 frames; notes go to `<out>/playtest_20261019.log`.
//! Nothing here asserts the game is "good", only that it does not crash.

use std::io::Write;

use bevy::prelude::*;
use rmc_client::controller::ControllerConfig;
use rmc_client::pickaxe::{PickTarget, SWING_BUTTON};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::shape::Shape;

const NAME: &str = "playtest_20261019";

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

fn settle(t: &mut TestApp) {
    for _ in 0..400 {
        t.step();
        let world = t.app().world();
        let stats = world.resource::<rmc_client::MeshStats>();
        if stats.initial_load_done
            && world.resource::<rmc_client::MeshQueue>().is_idle()
            && stats.awaiting_upload == 0
        {
            return;
        }
    }
    note("WARNING: meshing did not settle in 400 frames");
}

fn shot(t: &mut TestApp, what: &str) {
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, NAME, t.frame());
    let hud = t.hud();
    let p = t.player();
    note(format!(
        "[frame {}] {what}: feet {:.2},{:.2},{:.2} grounded {} yaw {:.2} pitch {:.2} | target {:?} {:?} damage {:?} inv {:?} pulse {:.2} break {:?} -> {}",
        t.frame(),
        p.pos.x,
        p.pos.y,
        p.pos.z,
        p.grounded,
        p.yaw,
        p.pitch,
        hud.target,
        hud.material,
        hud.damage,
        hud.inventory
            .iter()
            .map(|i| (i.name.clone(), i.count))
            .collect::<Vec<_>>(),
        hud.swing_pulse,
        hud.last_break.map(|b| (b.cell, b.fade)),
        path.display()
    ));
}

fn run(t: &mut TestApp, n: u32, what: &str) {
    for _ in 0..n {
        t.step();
        let pen = t.player_penetration();
        if !pen.is_empty() {
            note(format!("[frame {}] PENETRATION {pen:?}", t.frame()));
        }
        if t.frame().is_multiple_of(60) {
            shot(t, what);
        }
    }
}

fn surface_y(t: &TestApp, x: i32, z: i32) -> i32 {
    (0..128)
        .rev()
        .find(|&y| t.host_world().is_solid(CellPos::new(x, y, z)))
        .expect("a column with ground")
}

fn edit(t: &mut TestApp, seq: u32, op: EditOp, min: CellPos, max: CellPos) {
    t.send(ClientMsg::Edit(EditCommand {
        id: CommandId {
            issuer: t.player_id(),
            seq,
        },
        issuer: t.player_id(),
        op,
        shape: Shape::Box { min, max },
    }));
    t.steps(3);
}

fn turn(t: &mut TestApp, yaw_deg: f32, pitch_down_deg: f32) {
    let s = t
        .app()
        .world()
        .resource::<ControllerConfig>()
        .sensitivity_rad;
    t.look(-yaw_deg.to_radians() / s, pitch_down_deg.to_radians() / s);
    t.step();
}

/// Holds the swing until a break shows or `max` frames pass, logging the
/// HUD's feedback fields each frame. Returns frames used.
fn swing_log(t: &mut TestApp, max: u32, what: &str) -> u32 {
    t.mouse_down(SWING_BUTTON);
    let mut used = 0;
    let mut frames = Vec::new();
    for i in 0..max {
        t.step();
        used = i + 1;
        let h = t.hud();
        frames.push(format!(
            "{:.1}/{}/{}",
            h.swing_pulse,
            h.damage.map(|d| i64::from(d.dealt)).unwrap_or(-1),
            h.last_break
                .map(|b| format!("{:.2}", b.fade))
                .unwrap_or_default()
        ));
        if i % 20 == 19 {
            shot(t, what);
        }
        if h.last_break.is_some() {
            break;
        }
    }
    t.mouse_up(SWING_BUTTON);
    note(format!(
        "  {what}: {used} frames; pulse/damage/break-fade per frame: {}",
        frames.join(" ")
    ));
    used
}

#[test]
#[ignore]
fn playtest_20261019() {
    let _ = std::fs::remove_file(e2e::out_dir().join(format!("{NAME}.log")));
    let mut t = TestApp::new(TestConfig::default());
    e2e::run_scenario(&mut t, NAME, play);
}

fn play(t: &mut TestApp) {
    settle(t);
    let config = t.app().world().resource::<ControllerConfig>().clone();
    let s = config.sensitivity_rad;

    // 1. Drop in from 3 m above the middle column.
    let (cx, cz) = (128, 128);
    let sy = surface_y(t, cx, cz);
    note(format!("surface at ({cx},{cz}) is y={sy}"));
    let top_m = (sy + 1) as f32 * CELL_SIZE_M;
    t.spawn_player(Vec3::new(
        (cx as f32 + 0.5) * CELL_SIZE_M,
        top_m + 3.0,
        (cz as f32 + 0.5) * CELL_SIZE_M,
    ));
    run(t, 59, "falling and landing");
    shot(t, "landed");

    // 2. Four quarter turns, level.
    for i in 0..4 {
        turn(t, -90.0, 0.0);
        run(t, 5, &format!("level view, quarter turn {}", i + 1));
        shot(t, &format!("level view {}", i + 1));
    }
    turn(t, 0.0, -20.0);
    shot(t, "pitched up 20 degrees");
    turn(t, 0.0, -60.0);
    shot(t, "pitched up 80 degrees");
    turn(t, 0.0, 240.0);
    shot(t, "looking down as far as the mouse goes");
    t.look(0.0, (t.player().pitch + 0.1) / s);
    t.step();

    // 3. Walk forward two seconds.
    let start = t.player();
    t.press(KeyCode::KeyW);
    for _ in 0..120 {
        t.step();
        if t.frame().is_multiple_of(60) {
            shot(t, "walking");
        }
    }
    t.release(KeyCode::KeyW);
    t.steps(2);
    let p = t.player();
    note(format!(
        "walked {:.2} m horizontally in 120 frames, dy {:.2}",
        (p.pos - start.pos).xz().length(),
        p.pos.y - start.pos.y
    ));

    // 4. Topsoil dig: look at the ground ahead and swing, logging feedback.
    turn(t, 0.0, 40.0);
    shot(t, "looking at ground ahead before the swing");
    let n = swing_log(t, 40, "topsoil swing");
    note(format!("topsoil broke (or not) after {n} frames"));
    t.steps(30);
    shot(t, "after topsoil dig");

    // 5. Place the hardest diggable solid as a wall ahead and dig it.
    t.look(0.0, (t.player().pitch + 0.1) / s);
    t.step();
    let hardest = t
        .host_world()
        .materials()
        .iter()
        .filter(|d| d.phase == Phase::Solid && d.diggable)
        .max_by_key(|d| d.hardness)
        .map(|d| (d.id, d.key.clone(), d.hardness))
        .expect("a diggable solid");
    note(format!("hardest diggable: {hardest:?}"));
    let p = t.player();
    let ahead = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let spot = p.pos + ahead * 1.5;
    let eye_cell_y = (p.eye.y / CELL_SIZE_M).floor() as i32;
    let c = CellPos::new(
        (spot.x / CELL_SIZE_M).floor() as i32,
        eye_cell_y - 1,
        (spot.z / CELL_SIZE_M).floor() as i32,
    );
    edit(
        t,
        8000,
        EditOp::Set(hardest.0),
        CellPos::new(c.0.x - 2, c.0.y - 1, c.0.z),
        CellPos::new(c.0.x + 2, c.0.y + 2, c.0.z),
    );
    settle(t);
    shot(t, "hard wall ahead");
    if let Some(hit) = t.app().world().resource::<PickTarget>().0 {
        note(format!("target {:?} at {:.2} m", hit.cell, hit.distance));
        let n = swing_log(t, 90, "hard wall swing");
        note(format!("hard cell gone after {n} frames"));
        t.steps(10);
        shot(t, "after the hard dig, break effect fading");
    }

    // 6. Back right up to the wall: lantern falloff at 0.2-0.3 m.
    t.press(KeyCode::KeyW);
    t.steps(60);
    t.release(KeyCode::KeyW);
    t.steps(4);
    shot(t, "against the wall");

    // 7. A forward tunnel by hand: swing, walk, swing.
    for i in 0..3 {
        let n = swing_log(t, 120, &format!("tunnel swing {i}"));
        note(format!("tunnel cell {i} took {n} frames"));
        t.press(KeyCode::KeyW);
        t.steps(20);
        t.release(KeyCode::KeyW);
        t.steps(2);
    }
    shot(t, "inside tunnel");
    turn(t, 180.0, 0.0);
    t.steps(4);
    shot(t, "looking back out of the tunnel");
    turn(t, 180.0, 0.0);

    // 8. Place a cell on the player's own body: the host should skip it.
    let stone = t
        .host_world()
        .materials()
        .iter()
        .find(|d| d.phase == Phase::Solid && d.emissive == 0.0 && d.diggable)
        .map(|d| d.id)
        .unwrap_or(MaterialId::AIR);
    let own = CellPos::new(
        (t.player().pos.x / CELL_SIZE_M).floor() as i32,
        (t.player().pos.y / CELL_SIZE_M).floor() as i32 + 1,
        (t.player().pos.z / CELL_SIZE_M).floor() as i32,
    );
    let before = t.host_world().is_solid(own);
    edit(t, 9001, EditOp::Set(stone), own, own);
    run(t, 10, "cell placed inside the player");
    note(format!(
        "Set at body cell {own:?}: solid before {before}, after {}; penetration {:?}",
        t.host_world().is_solid(own),
        t.player_penetration()
    ));
    // A box Set around the player: cells outside the body should land.
    let (px, py, pz) = (own.0.x, own.0.y - 1, own.0.z);
    edit(
        t,
        9002,
        EditOp::Set(stone),
        CellPos::new(px - 2, py - 1, pz - 2),
        CellPos::new(px + 2, py + 2, pz + 2),
    );
    run(t, 10, "big Set around the player");
    note(format!(
        "after big Set: player {:?}, penetration {:?}",
        t.player(),
        t.player_penetration()
    ));
    shot(t, "boxed in");
    t.press(KeyCode::KeyW);
    run(t, 30, "walking while boxed in");
    t.release(KeyCode::KeyW);

    // 9. Fly camera over the terrain for the whole-world view.
    t.press(KeyCode::KeyV);
    t.steps(2);
    t.release(KeyCode::KeyV);
    turn(t, 0.0, 30.0);
    t.press(KeyCode::Space);
    t.steps(90);
    t.release(KeyCode::Space);
    shot(t, "fly camera high up looking down");
    assert_eq!(t.render_failure(), None);
}
