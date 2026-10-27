//! The season and weather on screen (`rmc_client::weather`): in a rainy
//! autumn the HUD chip names the season and the rain, and rain streaks fall
//! around the camera; in the dry winter the chip says frost and nothing
//! falls.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the weather set
//! up (the `Weather` resource, the chip's slot and lines, the drop count)
//! and that the frames render and differ; the screenshots are for the
//! playtester.

use bevy::prelude::*;
use rmc_client::weather::{MAX_DROPS, RainDrop, WeatherHud, WeatherLine};
use rmc_client::{
    GameCamera, HudAnchor, HudSlot, MeshQueue, MeshStats, Replica, SeasonSummary, Sky, Weather,
};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};

#[test]
#[ignore]
fn weather_shows() {
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ui: true,
        weather: true,
        tiles: true,
        size: (1280, 720),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "weather_shows", run);
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

fn set_season(t: &mut TestApp, season: u32) {
    t.app_mut()
        .world_mut()
        .resource_mut::<SeasonSummary>()
        .running = Some(season);
}

fn drops(t: &mut TestApp) -> usize {
    let world = t.app_mut().world_mut();
    world.query::<&RainDrop>().iter(world).count()
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

fn shoot(t: &mut TestApp, label: &str) -> image::RgbaImage {
    t.steps(3);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("weather_shows-{label}"), t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None, "{label} renders");
    image
}

fn run(t: &mut TestApp) {
    settle(t);
    // On the ground in the middle of the world, looking across it.
    let (x, z) = (32, 32);
    let ground = i32::from(
        t.app()
            .world()
            .resource::<Replica>()
            .world()
            .surface_y(x, z),
    );
    let eye = Vec3::new(x as f32, (ground + 1) as f32, z as f32) * CELL_SIZE_M
        + Vec3::new(0.25, 1.6, 0.25);
    {
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera =
            Transform::from_translation(eye).looking_at(eye + Vec3::new(1.0, -0.15, 0.6), Vec3::Y);
    }

    // Autumn, the wettest season: full rain.
    set_season(t, 2);
    t.steps(MAX_DROPS as u32 / 30);
    let w = t.app().world().resource::<Weather>().clone();
    assert_eq!(
        (w.key.as_str(), w.sky(), w.rain),
        ("autumn", Sky::Rain, 1.0)
    );
    assert_eq!(drops(t), MAX_DROPS, "full rain");
    {
        let world = t.app_mut().world_mut();
        let slots: Vec<HudSlot> = world
            .query_filtered::<&HudSlot, With<WeatherHud>>()
            .iter(world)
            .copied()
            .collect();
        assert_eq!(slots, [HudSlot(HudAnchor::BottomLeft)], "the chip");
    }
    let l = lines(t);
    assert_eq!(l[0], "Autumn · Year 1");
    assert!(l[1].ends_with("Heavy rain"), "{l:?}");
    let rain = shoot(t, "autumn-rain");

    // Winter is dry: no drops, frost.
    set_season(t, 3);
    t.steps(2);
    assert_eq!(drops(t), 0, "no rain in winter");
    assert!(lines(t)[1].ends_with("Frost"), "{:?}", lines(t));
    let winter = shoot(t, "winter");
    assert_ne!(rain, winter, "the rain shows");
}
