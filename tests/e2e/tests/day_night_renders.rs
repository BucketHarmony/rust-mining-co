//! Day and night on the game calendar (`docs/CALENDAR.md`): the host runs
//! the shipped `calendar.ron` an hour at a time and sends its clock; the
//! client puts the sun on its path, dims the light and darkens the sky by
//! night, and the HUD chip shows the day and the time.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the day set up
//! (the time, the sun light's direction and illuminance, the ambient
//! light, the fog and clear colors against `day_sky`, the chip's lines)
//! and that the frames render and differ; the screenshots (morning, noon,
//! sunset, night) are for the playtester.

use bevy::pbr::DistanceFog;
use bevy::prelude::*;
use rmc_client::render::Sun;
use rmc_client::weather::WeatherLine;
use rmc_client::{DayNight, GameCamera, GameTime, MeshQueue, MeshStats, Replica, day_sky};
use rmc_eco::Calendar;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};

#[test]
#[ignore]
fn day_night_renders() {
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ui: true,
        weather: true,
        tiles: true,
        size: (1280, 720),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "day_night_renders", run);
}

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

fn lines(t: &mut TestApp) -> Vec<String> {
    let world = t.app_mut().world_mut();
    let mut v: Vec<(usize, String)> = world
        .query::<(&WeatherLine, &Text)>()
        .iter(world)
        .map(|(l, s)| (l.0, s.0.clone()))
        .collect();
    v.sort();
    v.into_iter().map(|(_, s)| s).collect()
}

/// Runs hours until the clock reads `time_h`, then checks what the day
/// set up and shoots.
fn shoot_at(t: &mut TestApp, time_h: f32, label: &str) -> image::RgbaImage {
    for _ in 0..24 {
        if t.app().world().resource::<GameTime>().time_h(None) == Some(time_h) {
            break;
        }
        t.eco_hour();
    }
    let time = *t.app().world().resource::<GameTime>();
    assert_eq!(time.time_h(None), Some(time_h), "{label}");
    let clock = time.clock.expect("a clock");
    t.steps(3);
    let sky = t.app().world().resource::<DayNight>().0.expect("a day sky");
    // The sun's place does not depend on the clouds.
    let want = day_sky(time_h, clock.hours, clock.daylight, 0.0);
    assert_eq!(sky.sun, want.sun, "{label}");
    {
        let world = t.app_mut().world_mut();
        let (transform, light) = world
            .query_filtered::<(&Transform, &DirectionalLight), With<Sun>>()
            .single(world)
            .expect("one sun");
        assert!(
            transform.forward().as_vec3().distance(-sky.light_toward) < 1e-4,
            "{label}: the light shines from the sun or moon"
        );
        assert_eq!(light.illuminance, sky.illuminance, "{label}");
        let fog = world
            .query_filtered::<&DistanceFog, With<GameCamera>>()
            .single(world)
            .expect("one camera");
        assert_eq!(fog.color, sky.horizon(), "{label}");
        assert_eq!(world.resource::<ClearColor>().0, sky.horizon(), "{label}");
        assert_eq!(
            world.resource::<GlobalAmbientLight>().brightness,
            sky.ambient
        );
    }
    let l = lines(t);
    let hh = format!("{:02}:00", time_h as u32);
    assert!(l[1].starts_with(&hh), "{label}: {l:?}");
    assert!(l[0].starts_with("Spring · Day 1"), "{label}: {l:?}");
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("day_night_renders-{label}"), t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None, "{label} renders");
    image
}

fn run(t: &mut TestApp) {
    settle(t);
    // On a hill in the middle of the world, looking south and west across
    // it, where the sun goes down.
    let (x, z) = (32, 32);
    let ground = i32::from(
        t.app()
            .world()
            .resource::<Replica>()
            .world()
            .surface_y(x, z),
    );
    let eye = Vec3::new(x as f32, (ground + 1) as f32, z as f32) * CELL_SIZE_M
        + Vec3::new(0.25, 3.0, 0.25);
    {
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera =
            Transform::from_translation(eye).looking_at(eye + Vec3::new(-1.0, 0.12, 0.7), Vec3::Y);
    }
    assert_eq!(
        t.app().world().resource::<DayNight>().0,
        None,
        "no clock yet"
    );

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    t.set_calendar(Some(Calendar::load(dir).expect("calendar.ron loads")));
    let morning = shoot_at(t, 7.0, "morning");
    let noon = shoot_at(t, 13.0, "noon");
    let sunset = shoot_at(t, 19.0, "sunset");
    let night = shoot_at(t, 23.0, "night");
    let day = t.app().world().resource::<DayNight>().0.unwrap();
    assert_eq!(day.daylight, 0.0, "23:00 is night");
    assert_ne!(noon, night, "night is not noon");
    assert_ne!(morning, sunset, "the light moves");
}
