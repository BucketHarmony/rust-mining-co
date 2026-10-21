//! Water is drawn in its own pass (SAD-P2 §10): a pool's water lands on
//! liquid mesh entities, separate from the opaque chunk meshes, drawn with
//! the one alpha-blended liquid material. The playtester judges the look
//! from the saved screenshot.

use bevy::prelude::*;
use rmc_client::{GameCamera, LiquidMaterial, LiquidMesh, MeshQueue, MeshStats};
use rmc_testkit::{Fixture, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};

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

#[test]
#[ignore]
fn liquid_pass_pool() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "liquid_pass_pool", run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let site = t.fixture(Fixture::Pool);
    t.steps(3);
    settle(t);
    eprintln!("pool: {:?}", site.feature);

    // Above the pool's near corner, looking down at its middle.
    {
        let (min, max) = (site.feature.min.0, site.feature.max.0);
        let centre = Vec3::new(
            (min.x + max.x + 1) as f32 / 2.0,
            (max.y + 1) as f32,
            (min.z + max.z + 1) as f32 / 2.0,
        ) * CELL_SIZE_M;
        let eye = centre + Vec3::new(-2.5, 3.0, -2.5);
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
    }
    t.steps(2);

    // The water is on liquid mesh entities drawn with the shared blended
    // material.
    {
        let world = t.app_mut().world_mut();
        let handle = world
            .get_resource::<LiquidMaterial>()
            .expect("the liquid material exists once water is meshed")
            .0
            .clone();
        let blend = world
            .resource::<Assets<StandardMaterial>>()
            .get(&handle)
            .expect("liquid material asset")
            .alpha_mode;
        assert_eq!(blend, AlphaMode::Blend, "water is alpha-blended");
        let mut quads = 0;
        for (mesh, material) in world
            .query::<(&LiquidMesh, Option<&MeshMaterial3d<StandardMaterial>>)>()
            .iter(world)
        {
            let material =
                material.unwrap_or_else(|| panic!("{:?} has the liquid material", mesh.pos));
            assert_eq!(material.0, handle);
            quads += mesh.quads;
        }
        assert!(quads > 0, "the pool's water is meshed");
        let stats = world.resource::<MeshStats>();
        assert!(stats.chunks_with_liquid > 0 && stats.liquid_quads > 0);
    }

    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, "liquid_pass_pool", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "render failure: {:?}",
        t.render_failure()
    );
}
