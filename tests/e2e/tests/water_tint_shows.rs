//! Water's color follows its dissolved substances (SAD-P2 §10, F146): a
//! pool's liquid meshes carry the `eco.tint` colors once the first tint
//! row's substance is dissolved in it at full strength. The test asserts
//! the client's tint rows, the replica's amounts, the remesh, and the
//! liquid pass's tinted vertex colors; the screenshots are for the
//! playtester (rule 21).

use std::collections::BTreeMap;

use bevy::prelude::*;
use rmc_client::{
    ChunkMesher, ClientEco, GameCamera, GreedyMesher, LiquidMesh, MeshInput, MeshQueue, MeshStats,
    Replica, is_liquid_pass,
};
use rmc_testkit::{Fixture, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos, ChunkPos};
use rmc_world::edit::EditOp;
use rmc_world::shape::Shape;

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

/// Per chunk with a liquid mesh, the liquid pass's vertex colors as the
/// pipeline makes them from the replica (with the client's tint rows) and
/// as they would be without tint. The uploaded mesh itself lives in the
/// render world only, so the unit test `meshpipe::tests::
/// liquid_mesh_follows_dissolved_tint` covers that the pipeline uploads
/// the tinted one.
type Colors = BTreeMap<ChunkPos, (Vec<[f32; 4]>, Vec<[f32; 4]>)>;

fn liquid_colors(t: &mut TestApp) -> Colors {
    let world = t.app_mut().world_mut();
    let chunks: Vec<ChunkPos> = world
        .query::<&LiquidMesh>()
        .iter(world)
        .map(|m| m.pos)
        .collect();
    let tint = &world.resource::<ClientEco>().tint;
    let replica = world.resource::<Replica>().world();
    chunks
        .into_iter()
        .map(|pos| {
            let mesh = |input: Option<MeshInput>| {
                GreedyMesher.mesh_liquid(&input.expect("resident")).colors
            };
            let tinted = mesh(MeshInput::from_world_tinted(replica, pos, tint));
            let plain = mesh(MeshInput::from_world(replica, pos));
            (pos, (tinted, plain))
        })
        .collect()
}

#[test]
#[ignore]
fn water_tint_shows() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "water_tint_shows", run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let site = t.fixture(Fixture::Pool);
    t.steps(3);
    settle(t);
    eprintln!("pool: {:?}", site.feature);

    // The client reads the host's tint rows.
    let tint = t.eco_tables().substance_tints();
    assert!(!tint.is_empty(), "the shipped tables tint water");
    assert_eq!(t.app().world().resource::<ClientEco>().tint, tint);

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

    let pool_chunks: Vec<ChunkPos> = site.feature.chunks().collect();
    let clean = liquid_colors(t);
    let in_pool = |c: &Colors| -> Vec<ChunkPos> {
        pool_chunks
            .iter()
            .copied()
            .filter(|p| c.contains_key(p))
            .collect()
    };
    assert!(!in_pool(&clean).is_empty(), "the pool's water is meshed");
    for pos in in_pool(&clean) {
        let (tinted, plain) = &clean[&pos];
        assert_eq!(tinted, plain, "{pos:?}: clean water is untinted");
    }
    let before = t.screenshot();
    let path = e2e::save_screenshot(&before, "water_tint_shows-clean", t.frame());
    eprintln!("clean: {}", path.display());

    // The first row's substance, at full strength, through the pool.
    let row = tint[0];
    let jobs = t.app().world().resource::<MeshStats>().jobs_started;
    t.apply_edits(&[(
        EditOp::SetLayer {
            layer: row.layer,
            value: i32::from(row.full),
        },
        Shape::Box {
            min: site.feature.min,
            max: site.feature.max,
        },
    )]);
    settle(t);
    t.steps(2);

    // The replica holds it, the pool's chunks were meshed again, and their
    // water is tinted.
    let water: Vec<CellPos> = {
        let replica = t.app().world().resource::<Replica>().world();
        Shape::Box {
            min: site.feature.min,
            max: site.feature.max,
        }
        .cells()
        .filter(|&c| {
            replica
                .get_cell(c)
                .and_then(|id| replica.materials().get(id))
                .is_some_and(is_liquid_pass)
        })
        .collect()
    };
    assert!(!water.is_empty(), "the pool holds water");
    for &cell in &water {
        let replica = t.app().world().resource::<Replica>().world();
        assert_eq!(
            replica.layer(cell, row.layer),
            i32::from(row.full),
            "{cell:?}"
        );
    }
    let tinted_now = liquid_colors(t);
    let pool = in_pool(&tinted_now);
    assert_eq!(pool, in_pool(&clean), "the same chunks hold water");
    let remeshed = t.app().world().resource::<MeshStats>().jobs_started - jobs;
    assert!(
        remeshed >= pool.len() as u64,
        "the pool's {} chunks were meshed again ({remeshed} jobs)",
        pool.len()
    );
    for pos in &pool {
        let (tinted, plain) = &tinted_now[pos];
        assert_ne!(tinted, plain, "{pos:?}: the water is tinted");
    }
    let after = t.screenshot();
    let path = e2e::save_screenshot(&after, "water_tint_shows", t.frame());
    eprintln!("tinted: {}", path.display());
    assert!(before.as_raw() != after.as_raw(), "the frames differ");
    assert_eq!(t.render_failure(), None);
}
