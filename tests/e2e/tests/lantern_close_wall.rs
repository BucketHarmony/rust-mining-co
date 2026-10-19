//! The lantern at close range (SAD §11.3, F087): facing a rock wall is the
//! game's main view, so the lantern must not blow out close rock, and dark
//! materials must stay dark at every distance.
//!
//! Inside a closed chamber (no sun, ambient light and the lantern only),
//! the camera faces a basalt wall and a topsoil wall at 0.3 m and at 1.5 m.
//! The center block of basalt at 0.3 m is at most [`MAX_CLOSE_RATIO`] times
//! as bright as at 1.5 m and stays below [`CLOSE_CEILING`]; basalt stays
//! darker than topsoil at both distances, and the lantern still brightens
//! basalt at 1.5 m.

use bevy::prelude::*;
use image::RgbaImage;
use rmc_client::render::Lantern;
use rmc_client::{GameCamera, MeshQueue, MeshStats};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::MaterialDef;
use rmc_world::shape::Shape;

/// The close and the reading distance from the wall, in meters.
const CLOSE_M: f32 = 0.3;
const READING_M: f32 = 1.5;
/// Basalt's center block at [`CLOSE_M`] may be at most this many times as
/// bright as at [`READING_M`] (mean Rec. 709 luminance of the sRGB bytes).
const MAX_CLOSE_RATIO: f64 = 1.25;
/// Basalt's center block at [`CLOSE_M`] stays below this luminance: far
/// from near-white (255). On WARP, F087 measured 241.5 at 0.3 m and 130.2
/// at 1.5 m before the fix (400,000 lm at the eye), and 74.3 and 61.5
/// after it. 120 is about twice basalt's own color luminance (62.6): room
/// for adapter differences, while a lantern that washes basalt out to a
/// light gray fails.
const CLOSE_CEILING: f64 = 120.0;

/// The lantern raises basalt's center block at [`READING_M`] by at least
/// this much over the same view with the lantern off: it is still a light
/// the player sees. F087 measured 61.5 with it and 48.7 without on WARP;
/// this is about half that gain.
const MIN_LANTERN_GAIN: f64 = 6.0;

/// Half the side of the sampled pixel block around the image center.
const BLOCK_HALF_PX: i64 = 32;
/// Pixels this close to the center on both axes are the crosshair's and
/// are left out of the sample.
const CROSSHAIR_HALF_PX: i64 = 8;

/// The chamber's shell (outer box) and the two walls inside it. Each wall's
/// face toward -Z is at z = [`WALL_FACE_Z`] cells.
const SHELL_MIN: CellPos = CellPos::new(36, 98, 20);
const SHELL_MAX: CellPos = CellPos::new(91, 125, 75);
const WALL_FACE_Z: i32 = 56;
const BASALT_MIN: CellPos = CellPos::new(40, 99, WALL_FACE_Z);
const BASALT_MAX: CellPos = CellPos::new(61, 124, 60);
const TOPSOIL_MIN: CellPos = CellPos::new(66, 99, WALL_FACE_Z);
const TOPSOIL_MAX: CellPos = CellPos::new(87, 124, 60);
/// Eye height, in cells: the middle of the walls.
const EYE_Y: i32 = 112;

fn luminance(p: &image::Rgba<u8>) -> f64 {
    (0.2126 * f64::from(p.0[0])) + (0.7152 * f64::from(p.0[1])) + (0.0722 * f64::from(p.0[2]))
}

/// Mean luminance of the block around the image center, crosshair left out.
fn center_luminance(image: &RgbaImage) -> f64 {
    let (cx, cy) = (i64::from(image.width() / 2), i64::from(image.height() / 2));
    let mut sum = 0.0;
    let mut n = 0.0;
    for dy in -BLOCK_HALF_PX..BLOCK_HALF_PX {
        for dx in -BLOCK_HALF_PX..BLOCK_HALF_PX {
            if dx.abs() <= CROSSHAIR_HALF_PX && dy.abs() <= CROSSHAIR_HALF_PX {
                continue;
            }
            sum += luminance(image.get_pixel((cx + dx) as u32, (cy + dy) as u32));
            n += 1.0;
        }
    }
    sum / n
}

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

fn material(t: &TestApp, key: &str) -> MaterialDef {
    t.host_world()
        .materials()
        .iter()
        .find(|d| d.key == key)
        .unwrap_or_else(|| panic!("assets/materials.ron has {key}"))
        .clone()
}

/// The camera `distance_m` in front of the middle of the wall spanning
/// cells `min.x..=max.x`, looking at it along +Z.
fn face(t: &mut TestApp, min: CellPos, max: CellPos, distance_m: f32) {
    let x = (min.0.x + max.0.x + 1) as f32 * CELL_SIZE_M / 2.0;
    let y = (EYE_Y as f32 + 0.5) * CELL_SIZE_M;
    let z = WALL_FACE_Z as f32 * CELL_SIZE_M - distance_m;
    let world = t.app_mut().world_mut();
    let mut camera = world
        .query_filtered::<&mut Transform, With<GameCamera>>()
        .single_mut(world)
        .expect("one game camera");
    *camera = Transform::from_xyz(x, y, z).looking_to(Vec3::Z, Vec3::Y);
}

/// Faces the wall and returns the center block's luminance, saving the
/// screenshot as `lantern_close_wall-<label>-<frame>.png`.
fn shoot(t: &mut TestApp, label: &str, min: CellPos, max: CellPos, distance_m: f32) -> f64 {
    face(t, min, max, distance_m);
    t.steps(2);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("lantern_close_wall-{label}"), t.frame());
    assert!(
        t.render_failure().is_none(),
        "render failure: {:?}",
        t.render_failure()
    );
    let lum = center_luminance(&image);
    eprintln!("{label}: center luminance {lum:.1} ({})", path.display());
    lum
}

#[test]
#[ignore]
fn lantern_close_wall() {
    // 4 × 4 × 4 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(127, 127, 127)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "lantern_close_wall", run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let basalt = material(t, "basalt");
    let topsoil = material(t, "topsoil");

    // A closed chamber of basalt near the top of the world, hollowed out,
    // with the two walls standing on its floor. No sun reaches inside.
    let edits = [
        (EditOp::Set(basalt.id), SHELL_MIN, SHELL_MAX),
        (
            EditOp::Remove,
            CellPos::new(SHELL_MIN.0.x + 1, SHELL_MIN.0.y + 1, SHELL_MIN.0.z + 1),
            CellPos::new(SHELL_MAX.0.x - 1, SHELL_MAX.0.y - 1, SHELL_MAX.0.z - 1),
        ),
        (EditOp::Set(basalt.id), BASALT_MIN, BASALT_MAX),
        (EditOp::Set(topsoil.id), TOPSOIL_MIN, TOPSOIL_MAX),
    ];
    for (seq, (op, min, max)) in (1..).zip(edits) {
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
    settle(t);
    let host = t.host_world();
    let eye = |x: i32| CellPos::new(x, EYE_Y, WALL_FACE_Z);
    assert_eq!(host.get_cell(eye(50)), Some(basalt.id));
    assert_eq!(host.get_cell(eye(76)), Some(topsoil.id));
    assert!(
        !host.is_solid(CellPos::new(50, EYE_Y, WALL_FACE_Z - 1)),
        "the chamber is hollow in front of the walls"
    );

    let basalt_close = shoot(t, "basalt-close", BASALT_MIN, BASALT_MAX, CLOSE_M);
    let basalt_far = shoot(t, "basalt-reading", BASALT_MIN, BASALT_MAX, READING_M);
    let topsoil_close = shoot(t, "topsoil-close", TOPSOIL_MIN, TOPSOIL_MAX, CLOSE_M);
    let topsoil_far = shoot(t, "topsoil-reading", TOPSOIL_MIN, TOPSOIL_MAX, READING_M);

    // The same basalt view with the lantern off: the lantern still shows.
    {
        let world = t.app_mut().world_mut();
        for mut light in world
            .query_filtered::<&mut PointLight, With<Lantern>>()
            .iter_mut(world)
        {
            light.intensity = 0.0;
        }
    }
    let basalt_dark = shoot(t, "basalt-no-lantern", BASALT_MIN, BASALT_MAX, READING_M);
    assert!(
        basalt_far >= basalt_dark + MIN_LANTERN_GAIN,
        "the lantern brightens basalt at {READING_M} m: {basalt_far:.1} with it, \
         {basalt_dark:.1} without"
    );

    assert!(
        basalt_far > 5.0,
        "basalt is lit at the reading distance ({basalt_far:.1})"
    );
    assert!(
        basalt_close <= MAX_CLOSE_RATIO * basalt_far,
        "basalt at {CLOSE_M} m ({basalt_close:.1}) is at most {MAX_CLOSE_RATIO} × \
         as bright as at {READING_M} m ({basalt_far:.1})"
    );
    assert!(
        basalt_close < CLOSE_CEILING,
        "basalt at {CLOSE_M} m is not washed out: {basalt_close:.1} < {CLOSE_CEILING}"
    );
    assert!(
        basalt_far < topsoil_far,
        "basalt ({basalt_far:.1}) is darker than topsoil ({topsoil_far:.1}) at {READING_M} m"
    );
    assert!(
        basalt_close < topsoil_close,
        "basalt ({basalt_close:.1}) is darker than topsoil ({topsoil_close:.1}) at {CLOSE_M} m"
    );
    assert!(
        basalt_close < topsoil_far,
        "basalt up close ({basalt_close:.1}) stays darker than topsoil at {READING_M} m \
         ({topsoil_far:.1})"
    );
}
