//! A readable horizon (SAD §11.3, F088): looking level from the default
//! spawn, the world's edge is not a black void, and distance reads as fog.
//!
//! First, from above the world's -X edge (no player yet), looking across it,
//! the camera measures the fog color as rendered: the sky below the
//! horizon, past the far shell, is the fog color by construction, and
//! terrain at the far end, nearly all fog, must match it (the fog shader
//! agrees with the sky). It also checks that the sky has a sun where the
//! Sun shines from, and a gradient from the horizon up.
//!
//! Then the player stands at the default spawn (the middle column's top) and
//! looks level in four directions. In each frame the upper third is not
//! near-black, and terrain far away is closer to the fog color than terrain
//! near by. Pixels are classed as near or far by casting their camera ray
//! into the host world.

use bevy::prelude::*;
use image::{Rgba, RgbaImage};
use rmc_client::controller::ControllerConfig;
use rmc_client::render::{FOG_START_M, sun_direction};
use rmc_client::{GameCamera, MeshQueue, MeshStats};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::raycast;

/// Every this many pixels on both axes, a pixel is sampled.
const SAMPLE_PX: u32 = 4;

/// A sampled pixel whose ray hits terrain closer than this is near: no fog
/// reaches it.
const NEAR_M: f32 = FOG_START_M;
/// A sampled pixel whose ray hits terrain farther than this is far.
const FAR_M: f32 = 56.0;
/// Rays are cast this far; one that hits nothing sees the sky.
const RAY_M: f32 = 1000.0;

/// The upper third of a level view is near-black when its 10th-percentile
/// luminance is at most this. Before F088 the shell there was an RGB-25
/// slab (luminance about 25).
const NEAR_BLACK: f64 = 64.0;

/// Terrain at least this far away is at least 94% fog.
const FOGGED_M: f32 = 92.0;
/// Its mean color distance to the fog as the sky shows it is at most this.
const FOGGED_MAX_DISTANCE: f64 = 20.0;

/// Far terrain's mean color distance to the fog is at most this fraction
/// of near terrain's.
const MAX_FAR_TO_NEAR: f64 = 0.6;

/// Sky pixels below the horizon used for the fog reference are at least
/// this far below it (the sine of the angle), clear of the horizon texels
/// where the gradient starts.
const BELOW_HORIZON: f32 = -0.02;

/// Looking at the sun, the center block's mean luminance is at least this:
/// the disc is far brighter than white.
const SUN_MIN: f64 = 230.0;

/// Half the side of the center block, in pixels.
const BLOCK_HALF_PX: i64 = 6;

fn luminance(p: &Rgba<u8>) -> f64 {
    (0.2126 * f64::from(p.0[0])) + (0.7152 * f64::from(p.0[1])) + (0.0722 * f64::from(p.0[2]))
}

fn color_distance(p: &Rgba<u8>, q: [f64; 3]) -> f64 {
    (0..3)
        .map(|c| (f64::from(p.0[c]) - q[c]).powi(2))
        .sum::<f64>()
        .sqrt()
}

/// Mean luminance of the block around the image center.
fn center_luminance(image: &RgbaImage) -> f64 {
    let (cx, cy) = (i64::from(image.width() / 2), i64::from(image.height() / 2));
    let mut sum = 0.0;
    let mut n = 0.0;
    for dy in -BLOCK_HALF_PX..=BLOCK_HALF_PX {
        for dx in -BLOCK_HALF_PX..=BLOCK_HALF_PX {
            sum += luminance(image.get_pixel((cx + dx) as u32, (cy + dy) as u32));
            n += 1.0;
        }
    }
    sum / n
}

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

fn set_camera(t: &mut TestApp, transform: Transform) {
    let world = t.app_mut().world_mut();
    let mut camera = world
        .query_filtered::<&mut Transform, With<GameCamera>>()
        .single_mut(world)
        .expect("one game camera");
    *camera = transform;
}

/// One sampled pixel: its color, its ray's direction, and where the ray
/// hits terrain (None: it sees the sky).
struct Sample {
    color: Rgba<u8>,
    dir: Vec3,
    hit_m: Option<f32>,
}

/// Screenshots the current view (saved as `horizon_readable-<label>`) and
/// samples it: each sampled pixel's ray from the game camera, cast into the
/// host world.
fn shoot(t: &mut TestApp, label: &str) -> (RgbaImage, Vec<Sample>) {
    t.steps(2);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("horizon_readable-{label}"), t.frame());
    eprintln!("{label}: {}", path.display());
    assert_eq!(t.render_failure(), None);
    let world = t.app_mut().world_mut();
    let (camera, transform) = world
        .query_filtered::<(&Camera, &GlobalTransform), With<GameCamera>>()
        .single(world)
        .map(|(c, g)| (c.clone(), *g))
        .expect("one game camera");
    let mut samples = Vec::new();
    for y in (SAMPLE_PX / 2..image.height()).step_by(SAMPLE_PX as usize) {
        for x in (SAMPLE_PX / 2..image.width()).step_by(SAMPLE_PX as usize) {
            let ray = camera
                .viewport_to_world(&transform, Vec2::new(x as f32 + 0.5, y as f32 + 0.5))
                .expect("a ray through the pixel");
            let dir = ray.direction.as_vec3();
            let hit_m = raycast(t.host_world(), ray.origin, dir, RAY_M).map(|h| h.distance);
            samples.push(Sample {
                color: *image.get_pixel(x, y),
                dir,
                hit_m,
            });
        }
    }
    (image, samples)
}

fn mean_color(samples: &[&Sample]) -> [f64; 3] {
    let n = samples.len() as f64;
    let mut sum = [0.0; 3];
    for s in samples {
        for (c, slot) in sum.iter_mut().enumerate() {
            *slot += f64::from(s.color.0[c]);
        }
    }
    sum.map(|v| v / n)
}

#[test]
#[ignore]
fn horizon_readable() {
    let mut t = TestApp::new(TestConfig::default());
    e2e::run_scenario(&mut t, "horizon_readable", run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let bounds = t.host_world().bounds();
    let min = bounds.min.0;
    let max = bounds.max.0;
    let middle_m = Vec3::new(
        (min.x + max.x + 1) as f32 * CELL_SIZE_M / 2.0,
        (max.y + 1) as f32 * CELL_SIZE_M,
        (min.z + max.z + 1) as f32 * CELL_SIZE_M / 2.0,
    );

    // 1. From 8 m above the world's top, 4 m in from its -X edge, level
    // along +X: the sky below the horizon, past the far shell, is the fog
    // color as rendered.
    let above = middle_m + Vec3::Y * 8.0;
    let edge = Vec3::new(min.x as f32 * CELL_SIZE_M + 4.0, above.y, above.z);
    set_camera(
        t,
        Transform::from_translation(edge).looking_to(Vec3::X, Vec3::Y),
    );
    let (_, samples) = shoot(t, "fog-reference");
    let below: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.hit_m.is_none() && s.dir.y < BELOW_HORIZON)
        .collect();
    assert!(
        below.len() >= 20,
        "{} sampled pixels see the sky below the horizon",
        below.len()
    );
    let fog = mean_color(&below);
    let fog_spread = below
        .iter()
        .map(|s| color_distance(&s.color, fog))
        .fold(0.0, f64::max);
    eprintln!(
        "fog as rendered: {fog:.1?} over {} pixels (max deviation {fog_spread:.1})",
        below.len()
    );
    assert!(fog_spread < 6.0, "the sky below the horizon is one color");
    assert!(
        fog.iter().all(|&c| c > 100.0),
        "the fog is light: {fog:.1?}"
    );

    // Terrain at the far end is nearly all fog: it matches the sky's fog
    // color, so the fog shader and the sky agree.
    let fogged: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.hit_m.is_some_and(|d| d >= FOGGED_M))
        .collect();
    assert!(
        fogged.len() >= 20,
        "{} sampled pixels see terrain past {FOGGED_M} m",
        fogged.len()
    );
    let fogged_d = fogged
        .iter()
        .map(|s| color_distance(&s.color, fog))
        .sum::<f64>()
        / fogged.len() as f64;
    eprintln!(
        "terrain past {FOGGED_M} m: {fogged_d:.1} from the fog ({} px)",
        fogged.len()
    );
    assert!(
        fogged_d <= FOGGED_MAX_DISTANCE,
        "terrain past {FOGGED_M} m ({fogged_d:.1} from the fog) fades into the sky"
    );

    // The sky has a gradient: well above the horizon it is darker than the
    // fog below it.
    let high: Vec<&Sample> = samples
        .iter()
        .filter(|s| s.hit_m.is_none() && s.dir.y > 0.3)
        .collect();
    assert!(!high.is_empty(), "the top of the view sees the sky");
    let high_color = mean_color(&high);
    let lum = |c: [f64; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    eprintln!("sky above 17°: {high_color:.1?}");
    assert!(
        lum(high_color) + 10.0 < lum(fog),
        "the sky darkens upward: {high_color:.1?} above, {fog:.1?} at the horizon"
    );
    assert!(
        high_color[2] - high_color[0] > fog[2] - fog[0] + 10.0,
        "the sky is bluer upward: {high_color:.1?} above, {fog:.1?} at the horizon"
    );

    // The sun: looking where the Sun shines from, the center is the disc;
    // the same elevation the other way round is plain sky.
    let to_sun = sun_direction();
    set_camera(
        t,
        Transform::from_translation(above).looking_to(to_sun, Vec3::Y),
    );
    let (sun_shot, _) = shoot(t, "sun");
    let away = Vec3::new(-to_sun.x, to_sun.y, -to_sun.z);
    set_camera(
        t,
        Transform::from_translation(above).looking_to(away, Vec3::Y),
    );
    let (away_shot, _) = shoot(t, "away-from-sun");
    let (sun_lum, away_lum) = (center_luminance(&sun_shot), center_luminance(&away_shot));
    eprintln!("center luminance: {sun_lum:.1} toward the sun, {away_lum:.1} away");
    assert!(sun_lum >= SUN_MIN, "the sun disc is at the Sun's direction");
    assert!(
        away_lum + 40.0 < sun_lum,
        "the sun is only where the Sun shines from"
    );

    // 2. The player at the default spawn: on top of the middle column.
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

    for (i, name) in ["north", "east", "south", "west"].into_iter().enumerate() {
        if i > 0 {
            // A quarter turn right.
            t.look(std::f32::consts::FRAC_PI_2 / sensitivity, 0.0);
            t.step();
        }
        let (image, samples) = shoot(t, name);
        let pitch = t.player().pitch;
        assert!(pitch.abs() < 1e-4, "{name}: looking level, pitch {pitch}");

        // The upper third is not near-black.
        let mut upper: Vec<f64> = image
            .enumerate_pixels()
            .filter(|(_, y, _)| *y < image.height() / 3)
            .map(|(_, _, p)| luminance(p))
            .collect();
        upper.sort_by(f64::total_cmp);
        let p10 = upper[upper.len() / 10];
        let mean = upper.iter().sum::<f64>() / upper.len() as f64;
        eprintln!("{name}: upper third luminance p10 {p10:.1}, mean {mean:.1}");
        assert!(
            p10 > NEAR_BLACK,
            "{name}: the upper third is near-black (p10 luminance {p10:.1})"
        );

        // Far terrain is closer to the fog color than near terrain.
        // The far samples include the shell; they are not near-black either.
        let near: Vec<&Sample> = samples
            .iter()
            .filter(|s| s.hit_m.is_some_and(|d| d < NEAR_M))
            .collect();
        let far: Vec<&Sample> = samples
            .iter()
            .filter(|s| s.hit_m.is_some_and(|d| d > FAR_M))
            .collect();
        assert!(
            near.len() >= 20 && far.len() >= 20,
            "{name}: {} near and {} far samples",
            near.len(),
            far.len()
        );
        let mut far_lum: Vec<f64> = far.iter().map(|s| luminance(&s.color)).collect();
        far_lum.sort_by(f64::total_cmp);
        let far_p10 = far_lum[far_lum.len() / 10];
        eprintln!("{name}: far terrain and shell luminance p10 {far_p10:.1}");
        assert!(
            far_p10 > NEAR_BLACK,
            "{name}: far terrain and the shell are near-black (p10 luminance {far_p10:.1})"
        );
        let to_fog = |set: &[&Sample]| {
            set.iter()
                .map(|s| color_distance(&s.color, fog))
                .sum::<f64>()
                / set.len() as f64
        };
        let (near_d, far_d) = (to_fog(&near), to_fog(&far));
        eprintln!(
            "{name}: distance to fog {near_d:.1} near ({} px), {far_d:.1} far ({} px)",
            near.len(),
            far.len()
        );
        assert!(
            far_d <= MAX_FAR_TO_NEAR * near_d,
            "{name}: far terrain ({far_d:.1} from the fog) is not closer to the fog than \
             near terrain ({near_d:.1})"
        );
    }
}
