//! The game boots on the default world, meshes it and renders it (SAD §12.4).

use bevy::prelude::*;
use image::{Rgba, RgbaImage};
use rmc_client::{GameCamera, MeshQueue, MeshStats, Replica};
use rmc_testkit::{TestApp, TestConfig, e2e};

/// A pixel is sky when no channel differs from the sky reference by more.
const SKY_TOLERANCE: i32 = 24;

fn is_sky(p: &Rgba<u8>, sky: &Rgba<u8>) -> bool {
    (0..3).all(|c| (p.0[c] as i32 - sky.0[c] as i32).abs() <= SKY_TOLERANCE)
}

fn luminance(p: &Rgba<u8>) -> u32 {
    (2126 * p.0[0] as u32 + 7152 * p.0[1] as u32 + 722 * p.0[2] as u32) / 10_000
}

/// The per-channel median of the image.
fn median(image: &RgbaImage) -> Rgba<u8> {
    let mut out = [0u8; 4];
    for (c, slot) in out.iter_mut().enumerate() {
        let mut v: Vec<u8> = image.pixels().map(|p| p.0[c]).collect();
        v.sort_unstable();
        *slot = v[v.len() / 2];
    }
    Rgba(out)
}

#[test]
#[ignore]
fn boot_renders() {
    let mut t = TestApp::new(TestConfig::default());
    t.steps(120);

    let world = t.app().world();
    let stats = world.resource::<MeshStats>().clone();
    let dirty = world.get_resource::<Replica>().map(|r| r.dirty().len());
    let idle = world.resource::<MeshQueue>().is_idle();
    let chunks = world
        .get_resource::<Replica>()
        .map(|r| r.world().chunk_count());
    let shot = t.screenshot();
    let path = e2e::save_screenshot(&shot, "boot_renders", t.frame());
    eprintln!("screenshot: {}", path.display());
    eprintln!("mesh stats after 120 steps: {stats:?}");

    // The sky as rendered (tonemapped, with bloom): look straight up.
    {
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        let eye = camera.translation;
        *camera = Transform::from_translation(eye).looking_to(Vec3::Y, Vec3::Z);
    }
    let up = t.screenshot();
    let sky = median(&up);
    let sky_up = up.pixels().filter(|p| is_sky(p, &sky)).count();
    eprintln!("sky reference {sky:?}");

    let pixels = shot.pixels().count();
    let ground: Vec<&Rgba<u8>> = shot.pixels().filter(|p| !is_sky(p, &sky)).collect();
    eprintln!("non-sky pixels: {} of {pixels}", ground.len());
    let mut lum: Vec<u32> = ground.iter().map(|p| luminance(p)).collect();
    lum.sort_unstable();

    assert_eq!(t.render_failure(), None);
    assert_eq!(chunks, Some(256), "the replica holds the whole world");
    assert_eq!(dirty, Some(0), "no dirty chunks after 120 steps");
    assert!(idle, "the mesh queue drained in 120 steps: {stats:?}");
    assert!(stats.initial_load_done, "initial load finished: {stats:?}");
    assert!(
        stats.meshed_chunks >= 200,
        "MeshStats reports {} chunks meshed",
        stats.meshed_chunks
    );
    assert!(
        sky_up * 100 >= pixels * 99,
        "looking up sees only sky ({sky_up} of {pixels})"
    );
    assert!(
        ground.len() * 100 > pixels * 40,
        "only {} of {pixels} pixels are not sky",
        ground.len()
    );
    // Not a flat plane: lighting, AO and materials vary across the terrain.
    let spread = lum[lum.len() * 95 / 100] - lum[lum.len() * 5 / 100];
    assert!(
        spread >= 20,
        "terrain luminance p5..p95 spans only {spread}"
    );
}
