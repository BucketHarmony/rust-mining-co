//! Photographs the castle level (`rmc_app::castle`): the game's default
//! world with the castle, river, moat and mine built in, rendered the way
//! the game would load it from `build_castle`'s save. Not a SAD feature;
//! a preview of a built level.
//!
//! Shots go to `target/e2e/castle_preview/`. Env:
//! - `CASTLE_SIZE=1280x720`: frame size (default 960x540).
//! - `CASTLE_ORBIT=<frames>`: also save an orbit of the castle, that many
//!   frames, under `orbit/` (encode with ffmpeg).

use bevy::prelude::*;
use rmc_app::castle::{self, CASTLE};
use rmc_app::cli::DEFAULT_WORLD_SIZE;
use rmc_app::session::{DEFAULT_SEED, GameAssets, assets_dir};
use rmc_client::{GameCamera, MeshQueue, MeshStats};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::CELL_SIZE_M;

const NAME: &str = "castle_preview";

fn settle(t: &mut TestApp) {
    for _ in 0..400 {
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
    eprintln!("meshing did not settle in 400 steps");
}

/// A point in metres from cell coordinates (the cell's centre).
fn m(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x + 0.5, y + 0.5, z + 0.5) * CELL_SIZE_M
}

fn set_camera(t: &mut TestApp, eye: Vec3, at: Vec3) {
    let world = t.app_mut().world_mut();
    let mut camera = world
        .query_filtered::<&mut Transform, With<GameCamera>>()
        .single_mut(world)
        .expect("one game camera");
    *camera = Transform::from_translation(eye).looking_at(at, Vec3::Y);
}

#[test]
#[ignore]
fn castle_preview() {
    let size = std::env::var("CASTLE_SIZE")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        })
        .unwrap_or((960, 540));
    let bounds = DEFAULT_WORLD_SIZE.bounds();
    let assets = GameAssets::load(&assets_dir())
        .and_then(|a| a.with_world_bounds(bounds))
        .expect("game assets load");
    let built = castle::build(&assets, DEFAULT_SEED).expect("the castle builds");
    let (g, w, floor) = (
        built.ground as f32,
        built.water as f32,
        built.mine_floor as f32,
    );
    let top = built.mine_top;
    let hall = built.mine_hall;
    let mut t = TestApp::from_world(
        TestConfig {
            seed: DEFAULT_SEED,
            world_bounds: bounds,
            size,
            ..TestConfig::default()
        },
        built.world,
        assets.host_config(DEFAULT_SEED),
    );
    settle(&mut t);
    let dir = e2e::out_dir().join(NAME);
    std::fs::create_dir_all(&dir).expect("create the output directory");
    let (cx, cz) = (CASTLE.0 as f32, CASTLE.1 as f32);
    let (mx, my, mz) = (top.0.x as f32, top.0.y as f32, top.0.z as f32);
    let shots: [(&str, Vec3, Vec3); 10] = [
        (
            "01-overview",
            m(cx - 50.0, g + 40.0, cz + 50.0),
            m(cx, g, cz),
        ),
        (
            "02-gate-bridge",
            m(cx + 8.0, g + 12.0, cz - 30.0),
            m(cx, g + 3.0, cz - 16.0),
        ),
        (
            "03-moat-corner",
            m(cx + 30.0, g + 8.0, cz + 30.0),
            m(cx + 13.0, g + 4.0, cz + 13.0),
        ),
        (
            "04-courtyard",
            m(cx - 9.0, g + 21.0, cz + 12.0),
            m(cx + 5.0, g, cz - 9.0),
        ),
        (
            "05-river-canal",
            m(cx - 50.0, g + 30.0, cz - 30.0),
            m(20.0, w, cz),
        ),
        (
            "06-mine-head",
            m(mx - 16.0, my + 10.0, mz + 14.0),
            m(mx + 1.0, my + 3.0, mz),
        ),
        (
            "07-mine-stair",
            m(mx + 1.0, my + 3.0, mz),
            m(mx + 10.0, my - 8.0, mz),
        ),
        (
            "08-mine-hall",
            m(
                hall.min.0.x as f32 + 1.0,
                floor + 3.5,
                hall.min.0.z as f32 + 1.0,
            ),
            m(hall.max.0.x as f32, floor + 1.5, hall.max.0.z as f32),
        ),
        (
            "09-castle-from-mine",
            m(mx - 6.0, my + 12.0, mz + 6.0),
            m(cx, g + 8.0, cz),
        ),
        (
            "10-mine-drift",
            m(mx - 7.0, floor + 2.5, mz + 5.0),
            m(mx + 12.0, floor + 2.0, mz + 5.0),
        ),
    ];
    for (name, eye, at) in shots {
        set_camera(&mut t, eye, at);
        t.steps(2);
        settle(&mut t);
        let image = t.screenshot();
        image
            .save(dir.join(format!("{name}.png")))
            .expect("save a shot");
        eprintln!("{name}: eye {eye:?} at {at:?}");
    }
    if let Some(frames) = std::env::var("CASTLE_ORBIT")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
    {
        let orbit = dir.join("orbit");
        std::fs::create_dir_all(&orbit).expect("create the orbit directory");
        let at = m(cx, g + 4.0, cz);
        for i in 0..frames {
            let a = (210.0 + 360.0 * i as f32 / frames as f32).to_radians();
            let eye = at + Vec3::new(a.cos() * 30.0, 14.0, a.sin() * 30.0);
            set_camera(&mut t, eye, at);
            if i == 0 {
                settle(&mut t);
            }
            t.screenshot()
                .save(orbit.join(format!("{i:05}.png")))
                .expect("save an orbit frame");
        }
    }
}
