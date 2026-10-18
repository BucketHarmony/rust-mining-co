//! A player's walk through the real generated terrain (M6 playtest): fall
//! from the sky, look around, walk, jump, dig forward and down with the
//! pickaxe, and place cells with raw edit commands (there is no place
//! control yet). Screenshots every 60 frames; observations go to
//! `<out>/playtest_20261018.log`. Nothing here asserts the game is
//! "good", only that it does not crash or render-fail.

use std::io::Write;

use bevy::prelude::*;
use rmc_client::controller::ControllerConfig;
use rmc_client::pickaxe::{PickTarget, SWING_BUTTON};
use rmc_protocol::ClientMsg;
use rmc_testkit::bots::DigBot;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::shape::Shape;

fn note(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    eprintln!("{msg}");
    let path = e2e::out_dir().join("playtest_20261018.log");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open the log");
    writeln!(f, "{msg}").expect("write the log");
}

fn settle(t: &mut TestApp) {
    for _ in 0..240 {
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
    note("WARNING: meshing did not settle in 240 frames");
}

fn shot(t: &mut TestApp, what: &str) {
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, "playtest_20261018", t.frame());
    let hud = t.hud();
    let p = t.player();
    note(format!(
        "[frame {}] {what}: feet {:.2},{:.2},{:.2} grounded {} yaw {:.2} pitch {:.2} | target {:?} {:?} damage {:?} inv {:?} -> {}",
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
        hud.inventory.iter().map(|i| i.count).collect::<Vec<_>>(),
        path.display()
    ));
}

/// Steps `n` frames and screenshots on every 60th frame since the start.
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

#[test]
#[ignore]
fn playtest_20261018() {
    let _ = std::fs::remove_file(e2e::out_dir().join("playtest_20261018.log"));
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "playtest_20261018", play);
}

fn play(t: &mut TestApp) {
    settle(t);
    let config = t.app().world().resource::<ControllerConfig>().clone();

    // 1. Drop in from 3 m up in the middle of the terrain.
    let (cx, cz) = (32, 32);
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

    // 2. Look around: four quarter turns.
    for i in 0..2 {
        turn(t, -90.0, 0.0);
        run(t, 59, &format!("looking, quarter turn {}", i + 1));
        shot(t, "view");
    }
    // Look up at the sky and straight down, to see the pitch clamp.
    turn(t, 0.0, -120.0);
    shot(t, "looking up as far as the mouse goes");
    turn(t, 0.0, 240.0);
    shot(t, "looking down as far as the mouse goes");
    t.look(0.0, (t.player().pitch + 0.1) / config.sensitivity_rad);
    t.step();

    // 3. Walk forward two seconds, jumping when blocked.
    let start = t.player();
    t.press(KeyCode::KeyW);
    for _ in 0..120 {
        let before = t.player().pos;
        t.step();
        if t.frame().is_multiple_of(60) {
            shot(t, "walking");
        }
        let _ = before;
    }
    t.release(KeyCode::KeyW);
    t.steps(2);
    let p = t.player();
    note(format!(
        "walked {:.2} m horizontally in 120 frames (4.3 m/s would be {:.1} m), dy {:.2}",
        (p.pos - start.pos).xz().length(),
        config.walk_speed_mps * 2.0,
        p.pos.y - start.pos.y
    ));

    // 4. Face level, place the hardest diggable solid two cells ahead at
    // eye height, and dig it with the real swing input, reading the HUD's
    // damage progress as it goes.
    let s = config.sensitivity_rad;
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
    {
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
    }
    shot(t, "hard wall ahead");
    match t.app().world().resource::<PickTarget>().0 {
        Some(hit) => {
            note(format!("target {:?} at {:.2} m", hit.cell, hit.distance));
            t.mouse_down(SWING_BUTTON);
            for i in 0..90 {
                t.step();
                if i % 10 == 9 {
                    note(format!(
                        "  swing frame {}: HUD damage {:?}",
                        i + 1,
                        t.hud().damage
                    ));
                }
                if i == 29 || i == 59 {
                    shot(t, "mid-dig");
                }
                if t.hud().target != Some(hit.cell) {
                    note(format!("  target gone at swing frame {}", i + 1));
                    break;
                }
            }
            t.mouse_up(SWING_BUTTON);
            t.steps(3);
            shot(t, "after the hard dig");
        }
        None => note("hard wall: nothing under the crosshair"),
    }

    // 5. Dig the cell under the feet, then the ones beside it: does the
    // player fall in, and can it get out?
    let feet_cell = CellPos::new(
        (t.player().pos.x / CELL_SIZE_M).floor() as i32,
        ((t.player().pos.y - 0.01) / CELL_SIZE_M).floor() as i32,
        (t.player().pos.z / CELL_SIZE_M).floor() as i32,
    );
    note(format!("feet cell below is {feet_cell:?}"));
    turn(t, 0.0, 89.0);
    match DigBot::new(feet_cell).run(t) {
        Ok(r) => note(format!("dug under the feet: {r:?}")),
        Err(e) => note(format!("DigBot under feet failed: {e:?}")),
    }
    run(t, 60, "after digging under the feet");
    shot(t, "pit 1");
    for dz in [-1, 1] {
        let c = CellPos::new(feet_cell.0.x, feet_cell.0.y, feet_cell.0.z + dz);
        if t.app()
            .world()
            .resource::<rmc_client::Replica>()
            .world()
            .is_solid(c)
        {
            let r = DigBot::new(c).run(t);
            note(format!("dug neighbor {c:?}: {r:?}"));
        }
    }
    run(t, 60, "pit 2");
    let p = t.player();
    note(format!(
        "after the pit: feet y {:.2} (ground was {:.2}), grounded {}",
        p.pos.y, top_m, p.grounded
    ));
    // Try to jump out.
    t.press(KeyCode::Space);
    run(t, 40, "jumping out");
    t.release(KeyCode::Space);
    note(format!(
        "after jumping: feet y {:.2}, grounded {}",
        t.player().pos.y,
        t.player().grounded
    ));

    // 6. Place cells with raw edits (no place control exists): a one-cell
    // step and a two-cell wall in front of the player, then walk into
    // them.
    let stone = t
        .host_world()
        .materials()
        .iter()
        .find(|d| d.phase == Phase::Solid && d.emissive == 0.0 && d.diggable)
        .map(|d| d.id)
        .unwrap_or(MaterialId::AIR);
    let p = t.player();
    let ahead = Vec3::new(-p.yaw.sin(), 0.0, -p.yaw.cos());
    let spot = p.pos + ahead * 2.0;
    let base = CellPos::new(
        (spot.x / CELL_SIZE_M).floor() as i32,
        (p.pos.y / CELL_SIZE_M).round() as i32,
        (spot.z / CELL_SIZE_M).floor() as i32,
    );
    note(format!("placing a wall at {base:?}, material {stone:?}"));
    turn(t, 0.0, -75.0);
    edit(
        t,
        9000,
        EditOp::Set(stone),
        CellPos::new(base.0.x - 2, base.0.y, base.0.z),
        CellPos::new(base.0.x + 2, base.0.y + 1, base.0.z),
    );
    run(t, 30, "wall placed");
    shot(t, "wall in front");
    t.press(KeyCode::KeyW);
    run(t, 90, "walking into a 2-cell wall");
    t.release(KeyCode::KeyW);
    note(format!("against the wall: {:?}", t.player()));
    t.press(KeyCode::Space);
    run(t, 30, "jumping at the wall");
    t.release(KeyCode::Space);
    note(format!("after jump at wall: {:?}", t.player()));

    // Place a cell on top of the player's own feet.
    let own = CellPos::new(
        (t.player().pos.x / CELL_SIZE_M).floor() as i32,
        (t.player().pos.y / CELL_SIZE_M).floor() as i32 + 1,
        (t.player().pos.z / CELL_SIZE_M).floor() as i32,
    );
    edit(t, 9001, EditOp::Set(stone), own, own);
    run(t, 30, "cell placed inside the player");
    note(format!(
        "after placing a cell at the player's body {own:?}: {:?}, penetration {:?}",
        t.player(),
        t.player_penetration()
    ));
    t.press(KeyCode::KeyW);
    run(t, 30, "walking while embedded");
    t.release(KeyCode::KeyW);
    note(format!("after W while embedded: {:?}", t.player()));
    t.press(KeyCode::KeyS);
    run(t, 30, "backing out");
    t.release(KeyCode::KeyS);
    note(format!(
        "after S while embedded: {:?} pen {:?}",
        t.player(),
        t.player_penetration()
    ));
    shot(t, "end");
    assert_eq!(t.render_failure(), None);
}
