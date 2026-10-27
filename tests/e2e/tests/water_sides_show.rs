//! A block of water standing on the ground shows its sides as well as its
//! top: the liquid pass emits a face wherever water meets air (SAD-P2 §10),
//! and the shared liquid material draws both sides of every face. The
//! screenshot is for the playtester (CLAUDE.md rule 21).

use bevy::prelude::*;
use rmc_client::{GameCamera, LiquidMesh, MeshQueue, MeshStats, Replica};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::shape::Shape;

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
fn water_sides_show() {
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        tiles: true,
        size: (960, 540),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "water_sides_show", run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let (min_xz, max_xz) = (20, 25);
    let replica = t.app().world().resource::<Replica>().world();
    let ground = (min_xz..=max_xz)
        .flat_map(|x| (min_xz..=max_xz).map(move |z| (x, z)))
        .map(|(x, z)| i32::from(replica.surface_y(x, z)))
        .max()
        .expect("columns");
    let water = replica.materials().id("water").expect("water");
    let min = CellPos::new(min_xz, ground + 1, min_xz);
    let max = CellPos::new(max_xz, ground + 3, max_xz);
    t.send(ClientMsg::Edit(EditCommand {
        id: CommandId {
            issuer: t.player_id(),
            seq: 1000,
        },
        issuer: t.player_id(),
        op: EditOp::Set(water),
        shape: Shape::Box { min, max },
    }));
    t.steps(3);
    settle(t);

    // The water is meshed in the liquid pass. (Which faces it has is the
    // mesher's unit test, `liquid_top_face_at_fill_height`; the mesh data
    // lives in the render world here.)
    let world = t.app_mut().world_mut();
    let quads: u32 = world
        .query::<&LiquidMesh>()
        .iter(world)
        .map(|m| m.quads)
        .sum();
    assert!(quads >= 5, "the block's top and four sides: {quads} quads");

    let centre = Vec3::new(
        (min_xz + max_xz + 1) as f32 / 2.0,
        (ground + 2) as f32,
        (min_xz + max_xz + 1) as f32 / 2.0,
    ) * CELL_SIZE_M;
    for (label, eye) in [
        ("side", centre + Vec3::new(-5.0, 1.5, -3.5)),
        ("low", centre + Vec3::new(-4.5, -0.2, 1.0)),
    ] {
        {
            let world = t.app_mut().world_mut();
            let mut camera = world
                .query_filtered::<&mut Transform, With<GameCamera>>()
                .single_mut(world)
                .expect("one game camera");
            *camera = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
        }
        t.steps(3);
        let image = t.screenshot();
        let path = e2e::save_screenshot(&image, &format!("water_sides_show-{label}"), t.frame());
        eprintln!("screenshot: {}", path.display());
        assert_eq!(t.render_failure(), None, "{label} renders");
    }
}
