//! Emissive cells glow through the chunk material's extension (SAD §11.2):
//! the extension's shader compiles on the adapter, chunk meshes are drawn
//! with it, and turning its scale to zero darkens a glowing block.

use bevy::prelude::*;
use image::RgbaImage;
use rmc_client::render::Lantern;
use rmc_client::{
    ChunkEmissiveMaterial, ChunkMesh, EmissiveChunkMaterial, GameCamera, MeshQueue, MeshStats,
};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::Phase;
use rmc_world::shape::Shape;

fn luminance(p: &image::Rgba<u8>) -> f64 {
    (0.2126 * p.0[0] as f64) + (0.7152 * p.0[1] as f64) + (0.0722 * p.0[2] as f64)
}

/// Mean luminance of the middle fifth of the image (both axes).
fn center_luminance(image: &RgbaImage) -> f64 {
    let (w, h) = image.dimensions();
    let (x0, x1, y0, y1) = (w * 2 / 5, w * 3 / 5, h * 2 / 5, h * 3 / 5);
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            sum += luminance(image.get_pixel(x, y));
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

#[test]
#[ignore]
fn emissive_glow() {
    // 4 × 4 × 4 chunks, full height.
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(127, 127, 127));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "emissive_glow", run);
}

fn run(t: &mut TestApp) {
    settle(t);

    // The first emissive solid material in the table, and a plain one to
    // build with.
    let table = t.host_world().materials().clone();
    let glow = table
        .iter()
        .find(|d| d.phase == Phase::Solid && d.emissive > 0.0)
        .expect("assets/materials.ron has an emissive solid material");
    let wall = table
        .iter()
        .find(|d| d.phase == Phase::Solid && d.emissive == 0.0 && d.diggable)
        .expect("assets/materials.ron has a plain diggable solid material");
    eprintln!(
        "emissive material: {:?} (strength {}), walls: {:?}",
        glow.key, glow.emissive, wall.key
    );

    // A closed chamber near the top of the world: a solid block of `wall`,
    // hollowed out, with a 16 × 2 × 16-cell slab of `glow` on its floor. No
    // sun reaches inside, so the slab is lit only by the ambient light, as
    // in a cave.
    let edits = [
        (
            EditOp::Set(wall.id),
            CellPos::new(48, 100, 48),
            CellPos::new(79, 123, 79),
        ),
        (
            EditOp::Remove,
            CellPos::new(49, 101, 49),
            CellPos::new(78, 122, 78),
        ),
        (
            EditOp::Set(glow.id),
            CellPos::new(56, 101, 56),
            CellPos::new(71, 102, 71),
        ),
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
    assert_eq!(host.get_cell(CellPos::new(64, 102, 64)), Some(glow.id));
    let inside = host
        .get_cell(CellPos::new(64, 110, 64))
        .and_then(|id| table.get(id))
        .expect("a known material inside the chamber");
    assert_eq!(inside.phase, Phase::Air, "the chamber is hollow");

    // Inside the chamber, 3.5 m above the slab, looking straight down, with
    // the lantern off.
    {
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        let slab_top = 103.0 * CELL_SIZE_M;
        *camera = Transform::from_xyz(32.0, slab_top + 3.5, 32.0).looking_to(Vec3::NEG_Y, Vec3::Z);
        for mut light in world
            .query_filtered::<&mut PointLight, With<Lantern>>()
            .iter_mut(world)
        {
            light.intensity = 0.0;
        }
    }

    // Every chunk mesh is drawn with the one emissive material.
    let handle = {
        let world = t.app_mut().world_mut();
        let handle = world
            .get_resource::<EmissiveChunkMaterial>()
            .expect("a rendering app has the emissive chunk material")
            .0
            .clone();
        let mut chunks = 0;
        for (mesh, material) in world
            .query::<(&ChunkMesh, Option<&MeshMaterial3d<ChunkEmissiveMaterial>>)>()
            .iter(world)
        {
            let material =
                material.unwrap_or_else(|| panic!("{:?} has the emissive material", mesh.pos));
            assert_eq!(material.0, handle);
            chunks += 1;
        }
        assert!(chunks > 0, "chunks are meshed");
        handle
    };

    let lit = t.screenshot();
    let path = e2e::save_screenshot(&lit, "emissive_glow", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "render failure: {:?}",
        t.render_failure()
    );

    // The same view with the glow scaled to zero.
    t.app_mut()
        .world_mut()
        .resource_mut::<Assets<ChunkEmissiveMaterial>>()
        .get_mut(&handle)
        .expect("emissive material asset")
        .extension
        .emissive_scale = 0.0;
    t.steps(2);
    let unlit = t.screenshot();
    let path = e2e::save_screenshot(&unlit, "emissive_glow-off", t.frame());
    eprintln!("screenshot without glow: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "render failure: {:?}",
        t.render_failure()
    );

    let (on, off) = (center_luminance(&lit), center_luminance(&unlit));
    eprintln!("center luminance: glow {on:.1}, no glow {off:.1}");
    assert!(
        off > 10.0,
        "the slab is drawn and lit without glow ({off:.1})"
    );
    assert!(
        on > off + 20.0,
        "the glow brightens the slab: {on:.1} vs {off:.1} without it"
    );
}
