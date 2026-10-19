//! M8 player's walk (2026-10-20) through the default generated world.
//! Boots and records how the initial load uploads (frames, uploads per
//! frame, mesh memory), drops in at the spawn, opens the F1 editor (stats
//! panel, gizmos), walks and sprints, digs topsoil by hand, places cells
//! with the F1 editor brush by mouse click, digs a one-cell shaft straight
//! down, and flies up for the whole-world view.
//! Screenshots every 60 frames; notes go to `<out>/playtest_20261020.log`.
//! Nothing here asserts the game is "good", only that it does not crash.

use std::io::Write;

use bevy::prelude::*;
use rmc_client::controller::ControllerConfig;
use rmc_client::pickaxe::SWING_BUTTON;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos};

const NAME: &str = "playtest_20261020";

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

fn shot(t: &mut TestApp, what: &str) {
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, NAME, t.frame());
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
        hud.inventory
            .iter()
            .map(|i| (i.name.clone(), i.count))
            .collect::<Vec<_>>(),
        path.display()
    ));
}

/// Steps `n` frames, shooting on every 60th frame and noting penetration.
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

fn turn(t: &mut TestApp, yaw_deg: f32, pitch_down_deg: f32) {
    let s = t
        .app()
        .world()
        .resource::<ControllerConfig>()
        .sensitivity_rad;
    t.look(-yaw_deg.to_radians() / s, pitch_down_deg.to_radians() / s);
    t.step();
}

fn mesh_line(t: &TestApp) -> String {
    let s = t.app().world().resource::<rmc_client::MeshStats>();
    format!(
        "meshed {} (geometry {}), quads {}, mesh_bytes {} ({:.1} MiB), queued {}, in_flight {}, awaiting {}, uploads {}, last-frame uploads {}, discarded {}, load_done {}",
        s.meshed_chunks,
        s.chunks_with_geometry,
        s.quads,
        s.mesh_bytes,
        s.mesh_bytes as f64 / (1024.0 * 1024.0),
        s.queued,
        s.in_flight,
        s.awaiting_upload,
        s.uploads,
        s.uploads_last_frame,
        s.jobs_discarded,
        s.initial_load_done
    )
}

#[test]
#[ignore]
fn playtest_20261020() {
    let _ = std::fs::remove_file(e2e::out_dir().join(format!("{NAME}.log")));
    let mut t = TestApp::new(TestConfig::default());
    e2e::run_scenario(&mut t, NAME, play);
}

fn play(t: &mut TestApp) {
    // 0. The initial load: how many frames, and how the uploads come.
    let clock = std::time::Instant::now();
    let mut per_frame = Vec::new();
    let mut done_at = None;
    for i in 0..400 {
        t.step();
        let world = t.app().world();
        let stats = world.resource::<rmc_client::MeshStats>();
        per_frame.push(stats.uploads_last_frame);
        if stats.initial_load_done
            && world.resource::<rmc_client::MeshQueue>().is_idle()
            && stats.awaiting_upload == 0
        {
            done_at = Some(i + 1);
            break;
        }
    }
    note(format!(
        "initial load done after {done_at:?} steps ({:.0?} wall); uploads per frame: {per_frame:?}",
        clock.elapsed()
    ));
    note(format!("after load: {}", mesh_line(t)));

    // 1. Drop in at the middle column, from 3 m up.
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

    // 2. Four quarter turns; one frame each plus a pitched-down shot.
    for i in 0..4 {
        turn(t, -90.0, 0.0);
        t.steps(4);
        shot(t, &format!("level view {}", i + 1));
    }
    turn(t, 0.0, 35.0);
    shot(t, "pitched down 35 degrees");
    turn(t, 0.0, -35.0);

    // 3. The editor and its stats panel.
    t.press(KeyCode::F1);
    t.steps(2);
    t.release(KeyCode::F1);
    t.steps(10);
    shot(t, "F1 editor open");
    note(format!("editor open: {}", t.editor().open));
    for key in [KeyCode::F2, KeyCode::F3, KeyCode::F4] {
        t.press(key);
        t.steps(2);
        t.release(key);
    }
    t.steps(10);
    shot(t, "F1 editor with chunk, target and mesh-queue gizmos");
    for key in [KeyCode::F2, KeyCode::F3, KeyCode::F4] {
        t.press(key);
        t.steps(2);
        t.release(key);
    }
    t.press(KeyCode::F1);
    t.steps(2);
    t.release(KeyCode::F1);
    t.steps(4);
    note(format!("editor open after F1 again: {}", t.editor().open));

    // 4. Walk two seconds, sprint two seconds.
    let start = t.player();
    t.press(KeyCode::KeyW);
    run(t, 120, "walking");
    t.press(KeyCode::ShiftLeft);
    run(t, 120, "sprinting");
    t.release(KeyCode::ShiftLeft);
    t.release(KeyCode::KeyW);
    t.steps(2);
    let p = t.player();
    note(format!(
        "walk+sprint 240 frames: {:.2} m horizontally, dy {:.2}",
        (p.pos - start.pos).xz().length(),
        p.pos.y - start.pos.y
    ));

    // 5. Jump twice (held space).
    t.press(KeyCode::Space);
    run(t, 40, "jumping");
    t.release(KeyCode::Space);
    t.steps(30);

    // 6. Dig topsoil by hand, twice, looking steeply down at the ground.
    turn(t, 0.0, 20.0);
    note(format!(
        "pitch now {:.2}, target {:?}",
        t.player().pitch,
        t.hud().target
    ));
    shot(t, "before topsoil dig");
    for i in 0..2 {
        t.mouse_down(SWING_BUTTON);
        for _ in 0..30 {
            t.step();
            if t.hud().last_break.is_some() {
                break;
            }
        }
        t.mouse_up(SWING_BUTTON);
        t.steps(6);
        note(format!("hand dig {i}: inventory {:?}", t.inventory()));
        shot(t, &format!("after hand dig {i}"));
    }

    // 7. Place with the editor brush the way a player would: F1, Set (1),
    // Cell (4), aim at the ground, click; look for the cell to change.
    let tap = |t: &mut TestApp, key: KeyCode| {
        t.press(key);
        t.steps(2);
        t.release(key);
        t.steps(1);
    };
    tap(t, KeyCode::F1);
    tap(t, KeyCode::Digit1);
    tap(t, KeyCode::Digit4);
    for i in 0..4 {
        let hash0 = t.host_world().hash();
        let target = t.hud().target;
        t.click(MouseButton::Left);
        t.steps(6);
        note(format!(
            "brush click {i}: target {target:?}, world changed {}",
            t.host_world().hash() != hash0
        ));
        turn(t, 25.0, 0.0);
        t.steps(2);
    }
    shot(t, "after four brush clicks (editor open)");
    tap(t, KeyCode::F1);
    t.steps(10);
    shot(t, "editor closed, brush cells in view");

    // 8. A shaft by hand: look straight down and keep swinging.
    turn(t, 0.0, 80.0);
    for i in 0..4 {
        t.mouse_down(SWING_BUTTON);
        for _ in 0..60 {
            t.step();
            if t.hud().last_break.is_some() {
                break;
            }
        }
        t.mouse_up(SWING_BUTTON);
        t.steps(25);
        note(format!(
            "shaft swing {i}: feet y {:.2}, inventory {:?}",
            t.player().pos.y,
            t.inventory()
        ));
        shot(t, &format!("shaft {i}"));
    }
    turn(t, 0.0, -80.0);
    t.steps(4);
    shot(t, "looking level from inside the shaft");
    t.press(KeyCode::Space);
    run(t, 40, "jumping inside the shaft");
    t.release(KeyCode::Space);
    t.steps(10);

    // 9. Fly up for the whole-world view; mesh stats at the end.
    t.press(KeyCode::KeyV);
    t.steps(2);
    t.release(KeyCode::KeyV);
    turn(t, 0.0, 30.0);
    t.press(KeyCode::Space);
    run(t, 60, "flying up");
    shot(t, "fly camera, 60 frames up, looking down");
    t.press(KeyCode::ShiftLeft);
    run(t, 120, "flying up fast");
    t.release(KeyCode::Space);
    t.release(KeyCode::ShiftLeft);
    t.steps(4);
    shot(t, "fly camera high up looking down");
    note(format!("end: {}", mesh_line(t)));
    assert_eq!(t.render_failure(), None);
}
