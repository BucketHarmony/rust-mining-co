//! The world's edge fades into the sky (SAD §11.3, §11.6, F095).
//!
//! The player stands at the default spawn and looks level north, north-east
//! (a diagonal, where the old shell's top edge tilted), east, south and
//! west. Then the fly camera climbs out over the world's -X/-Z corner,
//! stops at the bounds plus `FLY_BOUNDS_MARGIN_M`, and looks down across
//! the world, with the fog thinned for its height.
//!
//! The look is the playtester's to judge from the screenshots (CLAUDE.md
//! rule 21). What is asserted is the setup (the clamp, the fog for the
//! height) and, coarsely, that no frame is blank and the high frame is not
//! a flat fill. The mesh's missing shell wall is asserted by
//! `render::tests::shell_above_surface_is_not_drawn_as_a_wall`.

use std::collections::BTreeSet;

use bevy::pbr::DistanceFog;
use bevy::prelude::*;
use image::RgbaImage;
use rmc_client::controller::ControllerConfig;
use rmc_client::flycam::FLY_BOUNDS_MARGIN_M;
use rmc_client::render::{distance_fog_at, linear_fog_range};
use rmc_client::{FLY_TOGGLE_KEY, FlyCam, GameCamera, MeshQueue, MeshStats};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos};

/// A frame with fewer distinct colors than this is blank (a flat fill).
const MIN_COLORS: usize = 16;

/// Steps until meshing is idle and nothing waits to upload.
fn settle(t: &mut TestApp) {
    for _ in 0..480 {
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

fn camera(t: &mut TestApp) -> Transform {
    let world = t.app_mut().world_mut();
    *world
        .query_filtered::<&Transform, With<GameCamera>>()
        .single(world)
        .expect("one game camera")
}

fn set_camera(t: &mut TestApp, transform: Transform) {
    let world = t.app_mut().world_mut();
    let mut camera = world
        .query_filtered::<&mut Transform, With<GameCamera>>()
        .single_mut(world)
        .expect("one game camera");
    *camera = transform;
}

fn camera_fog(t: &mut TestApp) -> Option<(f32, f32)> {
    let world = t.app_mut().world_mut();
    let fog = world
        .query_filtered::<&DistanceFog, With<GameCamera>>()
        .single(world)
        .expect("the game camera has fog");
    linear_fog_range(&fog.falloff)
}

fn distinct_colors(image: &RgbaImage) -> usize {
    image
        .pixels()
        .map(|p| [p.0[0], p.0[1], p.0[2]])
        .collect::<BTreeSet<_>>()
        .len()
}

/// Screenshots the current view as `horizon_no_box_edge-<label>` and checks
/// it is not blank.
fn shoot(t: &mut TestApp, label: &str) -> RgbaImage {
    t.steps(2);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("horizon_no_box_edge-{label}"), t.frame());
    assert_eq!(t.render_failure(), None);
    let colors = distinct_colors(&image);
    eprintln!("{label}: {} ({colors} colors)", path.display());
    assert!(
        colors >= MIN_COLORS,
        "{label}: a blank frame ({colors} colors)"
    );
    image
}

#[test]
#[ignore]
fn horizon_no_box_edge() {
    let mut t = TestApp::new(TestConfig::default());
    e2e::run_scenario(&mut t, "horizon_no_box_edge", run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let bounds = t.host_world().bounds();
    let (min, max) = (bounds.min.0, bounds.max.0);

    // The player at the default spawn: on top of the middle column.
    let (cx, cz) = (
        min.x + (max.x - min.x + 1) / 2,
        min.z + (max.z - min.z + 1) / 2,
    );
    let top = (min.y..=max.y)
        .rev()
        .find(|&y| t.host_world().is_solid(CellPos::new(cx, y, cz)))
        .expect("the middle column has ground");
    let feet = Vec3::new(
        (cx as f32 + 0.5) * CELL_SIZE_M,
        (top + 1) as f32 * CELL_SIZE_M,
        (cz as f32 + 0.5) * CELL_SIZE_M,
    );
    t.spawn_player(feet);
    t.steps(30);
    assert!(
        t.player().grounded,
        "standing at the spawn: {:?}",
        t.player()
    );
    settle(t);
    let sensitivity = t
        .app()
        .world()
        .resource::<ControllerConfig>()
        .sensitivity_rad;

    // Level views, turning right: the four horizon_readable directions and
    // one diagonal between the first two.
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
    let views = [
        ("north", 0.0),
        ("northeast", FRAC_PI_4),
        ("east", FRAC_PI_4),
        ("south", FRAC_PI_2),
        ("west", FRAC_PI_2),
    ];
    for (name, turn) in views {
        if turn > 0.0 {
            t.look(turn / sensitivity, 0.0);
            t.step();
        }
        let pitch = t.player().pitch;
        assert!(pitch.abs() < 1e-4, "{name}: looking level, pitch {pitch}");
        // On the ground the fog is the ground-level fog.
        assert_eq!(
            camera_fog(t),
            linear_fog_range(&distance_fog_at(0.0).falloff),
            "{name}"
        );
        shoot(t, name);
    }

    // The fly camera, just under its ceiling outside the -X/-Z corner,
    // looking across the world and down. Climbing, it stops at the bounds
    // plus the margin.
    t.press(FLY_TOGGLE_KEY);
    t.step();
    t.release(FLY_TOGGLE_KEY);
    t.step();
    assert!(
        t.app().world().resource::<FlyCam>().enabled,
        "fly mode is on"
    );
    let top_m = (max.y + 1) as f32 * CELL_SIZE_M;
    let ceiling = top_m + FLY_BOUNDS_MARGIN_M;
    let corner = min.as_vec3() * CELL_SIZE_M - Vec3::new(16.0, 0.0, 16.0);
    let middle = (min + max + IVec3::ONE).as_vec3() * CELL_SIZE_M / 2.0;
    let eye = Vec3::new(corner.x, ceiling - 2.0, corner.z);
    let across = Vec3::new(middle.x - eye.x, 0.0, middle.z - eye.z).normalize();
    let pitch = (-35f32).to_radians();
    let look = across * pitch.cos() + Vec3::Y * pitch.sin();
    set_camera(
        t,
        Transform::from_translation(eye).looking_to(look, Vec3::Y),
    );
    t.press(KeyCode::ShiftLeft);
    t.press(KeyCode::Space);
    t.steps(10);
    t.release(KeyCode::Space);
    t.release(KeyCode::ShiftLeft);
    t.step();
    let high = camera(t);
    eprintln!("fly camera at {:?}", high.translation);
    assert!(
        (high.translation.y - ceiling).abs() < 1e-3,
        "the fly camera stops {FLY_BOUNDS_MARGIN_M} m over the world's top: y {} m, \
         the top {top_m} m",
        high.translation.y
    );
    assert!((high.translation.x - eye.x).abs() < 1e-3 && (high.translation.z - eye.z).abs() < 1e-3);
    // Up there the fog is thinned for the height.
    assert_eq!(
        camera_fog(t),
        linear_fog_range(&distance_fog_at(FLY_BOUNDS_MARGIN_M).falloff),
        "the fog at the fly camera's ceiling"
    );
    shoot(t, "fly-high");
}
