//! Chunk faces draw with the tile textures of `assets/tiles.ron`
//! (`rmc_client::tiles`), and the editor's Reload-tiles action (F7) loads
//! them again.
//!
//! Look and feel (CLAUDE.md rule 21): the test asserts what the code set up
//! (the chunk material holds the tile array and a lookup that points the
//! shipped materials' faces at their layers) and that the frames render; the
//! screenshots are for the playtester.

use bevy::prelude::*;
use rmc_client::material::{ChunkEmissiveMaterial, EmissiveChunkMaterial};
use rmc_client::tiles::{LoadedTiles, TileDir};
use rmc_client::{ClientMaterials, GameCamera, MeshQueue, MeshStats};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::CELL_SIZE_M;

#[test]
#[ignore]
fn tiles_render() {
    let mut t = TestApp::new(TestConfig {
        tiles: true,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "tiles_render", run);
}

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

fn shoot(t: &mut TestApp, label: &str) {
    t.steps(4);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("tiles_render-{label}"), t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None, "{label} renders");
}

/// Asserts the chunk material holds the loaded tile set: the array texture
/// and a lookup whose entries point at its layers.
fn assert_material_has_tiles(t: &TestApp) {
    let world = t.app().world();
    let loaded = &world.get_resource::<LoadedTiles>().expect("tiles loaded").0;
    assert!(!loaded.layers.is_empty(), "the shipped tiles decode");
    let handle = &world.resource::<EmissiveChunkMaterial>().0;
    let material = world
        .resource::<Assets<ChunkEmissiveMaterial>>()
        .get(handle)
        .expect("the chunk material exists");
    assert!(material.extension.tiles.is_some(), "the tile array is bound");
    let table = &world.resource::<ClientMaterials>().0;
    assert_eq!(
        material.extension.tile_lookup,
        loaded.lookup(table),
        "the lookup is the loaded set's"
    );
    let layers = loaded.layers.len() as u32;
    let mut tiled = 0;
    for (key, faces) in &loaded.faces {
        let id = usize::from(table.id(key).expect("tiles.ron names a material").0);
        let slots = material.extension.tile_lookup.faces[id];
        for (face, slot) in faces.iter().zip([slots.x, slots.y, slots.z]) {
            assert_eq!(slot, face.map_or(0, |l| l + 1), "{key}");
            assert!(slot <= layers, "{key} points past the array");
        }
        tiled += usize::from(slots.x > 0);
    }
    assert!(tiled > 0, "some material has a top tile");
}

fn run(t: &mut TestApp) {
    assert!(t.app().world().get_resource::<TileDir>().is_some());
    settle(t);
    assert_material_has_tiles(t);

    // Over the middle of the world, a few meters above the ground, looking
    // across it and then down at it.
    let bounds = t.host_world().bounds();
    let (min, max) = (bounds.min.0, bounds.max.0);
    let (x, z) = ((min.x + max.x) / 2, (min.z + max.z) / 2);
    let ground = i32::from(t.host_world().surface_y(x, z));
    let eye = Vec3::new(x as f32, (ground + 6) as f32, z as f32) * CELL_SIZE_M;
    set_camera(
        t,
        Transform::from_translation(eye).looking_to(Vec3::new(1.0, -0.25, 0.3), Vec3::Y),
    );
    shoot(t, "across");
    set_camera(
        t,
        Transform::from_translation(eye).looking_to(Vec3::new(0.2, -1.0, 0.1), Vec3::Y),
    );
    shoot(t, "down");

    // F7's action reloads the set and applies it again.
    t.editor().tiles_reload_requested = true;
    t.step();
    assert!(!t.editor().tiles_reload_requested, "the request is taken");
    assert_material_has_tiles(t);
    shoot(t, "reloaded");
}
