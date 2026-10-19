//! The fly camera (noclip) moves through solid cells at its own speed
//! (SAD §11.6, §12.4).

use bevy::prelude::*;
use rmc_client::{FLY_TOGGLE_KEY, FlyCam, GameCamera, MeshStats, Replica};
use rmc_testkit::{TestApp, TestConfig, e2e, frame_duration};
use rmc_world::coords::{CellAabb, CellPos};

/// Allowed difference between a frame's move and speed × direction × dt.
const EPS_M: f32 = 1e-3;

fn camera(t: &mut TestApp) -> Transform {
    let world = t.app_mut().world_mut();
    *world
        .query_filtered::<&Transform, With<GameCamera>>()
        .single(world)
        .expect("one game camera")
}

fn fly(t: &TestApp) -> FlyCam {
    t.app().world().resource::<FlyCam>().clone()
}

fn is_solid(t: &TestApp, p: Vec3) -> bool {
    t.app()
        .world()
        .resource::<Replica>()
        .world()
        .is_solid(CellPos::from_meters(p))
}

/// One leg: holds `keys` for `frames` steps and checks that every frame
/// moves the camera by exactly `dir` × speed × dt, whatever it passes
/// through. Returns how many frames ended inside a solid cell.
fn leg(t: &mut TestApp, name: &str, keys: &[KeyCode], dir: Vec3, fast: bool, frames: u32) -> u32 {
    let cfg = fly(t);
    let speed = cfg.speed_mps * if fast { cfg.fast_multiplier } else { 1.0 };
    let step_m = dir.normalize() * speed * frame_duration().as_secs_f32();
    for &k in keys {
        t.press(k);
    }
    let mut solid = 0;
    let mut prev = camera(t).translation;
    for i in 0..frames {
        t.step();
        let now = camera(t).translation;
        let moved = now - prev;
        assert!(
            (moved - step_m).length() < EPS_M,
            "{name} frame {i}: moved {moved:?}, expected {step_m:?} (at {now:?})"
        );
        if is_solid(t, now) {
            solid += 1;
        }
        prev = now;
    }
    for &k in keys {
        t.release(k);
    }
    t.step();
    let after = camera(t).translation;
    assert!(
        (after - prev).length() < EPS_M,
        "{name}: the camera stops when the keys are released ({prev:?} -> {after:?})"
    );
    eprintln!("{name}: {solid} of {frames} frames inside solid cells, now at {after:?}");
    solid
}

#[test]
#[ignore]
fn fly_camera() {
    // 4 × 4 × 4 chunks, full height: surface, caves and bedrock.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(127, 127, 127)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "fly_camera", run);
}

fn run(t: &mut TestApp) {
    for _ in 0..120 {
        if t.app().world().resource::<MeshStats>().initial_load_done {
            break;
        }
        t.step();
    }
    assert!(t.app().world().resource::<MeshStats>().initial_load_done);

    // The toggle key turns the fly camera on.
    let enabled = fly(t).enabled;
    t.press(FLY_TOGGLE_KEY);
    t.step();
    t.release(FLY_TOGGLE_KEY);
    t.step();
    assert_eq!(fly(t).enabled, !enabled, "the toggle key flips fly mode");
    if !fly(t).enabled {
        t.press(FLY_TOGGLE_KEY);
        t.step();
        t.release(FLY_TOGGLE_KEY);
        t.step();
    }
    assert!(fly(t).enabled, "fly mode is on");

    let start = camera(t);
    eprintln!("start: {start:?}");
    assert!(!is_solid(t, start.translation), "starts in the air");

    // Forward, fast, along the look direction (20° down) into the world.
    let forward = start.forward().as_vec3();
    let mut solid = leg(
        t,
        "forward",
        &[KeyCode::ShiftLeft, KeyCode::KeyW],
        forward,
        true,
        30,
    );
    // Straight down, fast, through the surface into the rock.
    let down = leg(
        t,
        "down",
        &[KeyCode::ShiftLeft, KeyCode::ControlLeft],
        Vec3::NEG_Y,
        true,
        60,
    );
    assert!(
        down >= 20,
        "the descent passes through solid ground ({down} frames inside solid cells)"
    );
    solid += down;
    // Sideways through the rock, at walking-pace fly speed.
    let right = camera(t).right().as_vec3();
    assert!(right.y.abs() < 1e-4, "strafing is horizontal");
    let side = leg(t, "right", &[KeyCode::KeyD], right, false, 60);
    assert!(
        side >= 1,
        "strafing underground passes through solid cells ({side} frames)"
    );
    solid += side;
    // Up a little, still inside the world.
    solid += leg(t, "up", &[KeyCode::Space], Vec3::Y, false, 30);

    let end = camera(t);
    eprintln!("end: {end:?}; {solid} frames inside solid cells");
    assert!(solid >= 30, "{solid} frames inside solid cells");
    assert!(
        end.translation.y < start.translation.y - 30.0,
        "the camera went deep into the world: {:?}",
        end.translation
    );
    // Moving does not turn the camera.
    assert!((end.forward().as_vec3() - forward).length() < 1e-4);

    // The mouse turns it and does not move it.
    t.look(-200.0, 0.0);
    t.step();
    let turned = camera(t);
    assert!(
        (turned.translation - end.translation).length() < EPS_M,
        "looking does not move the camera"
    );
    let expected = Quat::from_rotation_y(200.0 * fly(t).sensitivity_rad) * end.forward().as_vec3();
    assert!(
        (turned.forward().as_vec3() - expected).length() < 1e-3,
        "mouse left turns left about +Y by dx × sensitivity: {:?} -> {:?}, expected {expected:?}",
        end.forward(),
        turned.forward()
    );

    let shot = t.screenshot();
    let path = e2e::save_screenshot(&shot, "fly_camera", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None);
}
