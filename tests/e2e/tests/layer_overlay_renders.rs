//! The layer overlay (SAD-P2 §10, §12, F161): over a pool, the editor's
//! overlay key picks the moisture layer, the client meshes every resident
//! chunk again in the layer's colormap, and the frame differs from the
//! material render.
//!
//! SAD-P2 §12 also asks for "mean blue over the pool region higher". Per
//! rule 21 that pixel check is asserted semantically instead: the overlay
//! reads the replica's moisture, which is higher in the pool than in the
//! ground around it, and the moisture colormap's blue rises with the
//! value, so the pool's water takes the colormap's wet end and the ground
//! a drier color. The pool's liquid meshes and the ground's opaque meshes
//! carry the overlay colors in place of the material colors. The
//! screenshots are for the playtester.

use std::collections::BTreeSet;

use bevy::prelude::*;
use rmc_client::{
    ChunkMesher, ColorSource, Colormap, EditorAction, GameCamera, GreedyMesher, LiquidMesh,
    MeshColorSource, MeshInput, MeshQueue, MeshStats, OVERLAY_LAYERS, Replica, TOGGLE_EDITOR_KEY,
    is_liquid_pass,
};
use rmc_testkit::{Fixture, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos, ChunkPos};
use rmc_world::layers::LayerId;
use rmc_world::shape::Shape;

/// The overlay layer under test.
const LAYER: LayerId = LayerId::Moisture;

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

/// Presses and releases `key`, one step each.
fn tap(t: &mut TestApp, key: KeyCode) {
    t.press(key);
    t.step();
    t.release(key);
    t.step();
}

/// The key bound to `action` in the editor's bindings.
fn key_of(action: EditorAction) -> KeyCode {
    rmc_client::EDITOR_BINDINGS
        .iter()
        .find(|&&(_, a)| a == action)
        .map(|&(k, _)| k)
        .expect("the action has a key")
}

/// The opaque and liquid vertex colors of chunk `pos` as the pipeline
/// meshes them from the replica with color source `source`. The uploaded
/// meshes live in the render world only; the unit test
/// `editor::tests::overlay_pick_triggers_remesh` covers that the pipeline
/// uploads these.
fn colors(t: &TestApp, pos: ChunkPos, source: ColorSource) -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
    let world = t.app().world();
    let replica = world.resource::<Replica>().world();
    let tint = &world.resource::<rmc_client::ClientEco>().tint;
    let input = MeshInput::from_world_colored(replica, pos, tint, source).expect("resident");
    (
        GreedyMesher.mesh(&input).colors,
        GreedyMesher.mesh_liquid(&input).colors,
    )
}

fn replica_layer(t: &TestApp, cell: CellPos) -> i32 {
    t.app()
        .world()
        .resource::<Replica>()
        .world()
        .layer(cell, LAYER)
}

#[test]
#[ignore]
fn layer_overlay_renders() {
    let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63));
    let mut t = TestApp::new(TestConfig {
        world_bounds: bounds,
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "layer_overlay_renders", run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let site = t.fixture(Fixture::Pool);
    t.steps(3);
    settle(t);
    let pool = site.feature;
    eprintln!("pool: {pool:?}");

    // The pool's water, and the ground's top cells in a ring around it.
    let water: Vec<CellPos> = {
        let replica = t.app().world().resource::<Replica>().world();
        Shape::Box {
            min: pool.min,
            max: pool.max,
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
    let (min, max) = (pool.min.0, pool.max.0);
    let y = max.y;
    let ring: Vec<CellPos> = (min.x - 1..=max.x + 1)
        .flat_map(|x| (min.z - 1..=max.z + 1).map(move |z| CellPos::new(x, y, z)))
        .filter(|c| !pool.contains(*c))
        .collect();
    {
        let replica = t.app().world().resource::<Replica>().world();
        for &c in &ring {
            assert!(replica.is_solid(c), "{c:?}: the ground rings the pool");
        }
    }

    // Above the pool's near corner, looking down at its middle, with the
    // ground around it in frame.
    {
        let centre = Vec3::new(
            (min.x + max.x + 1) as f32 / 2.0,
            (max.y + 1) as f32,
            (min.z + max.z + 1) as f32 / 2.0,
        ) * CELL_SIZE_M;
        let eye = centre + Vec3::new(-3.5, 4.0, -3.5);
        let world = t.app_mut().world_mut();
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_translation(eye).looking_at(centre, Vec3::Y);
    }
    t.steps(2);

    // The material render.
    assert_eq!(
        t.app().world().resource::<MeshColorSource>().0,
        ColorSource::Material,
        "the meshes start in material colors"
    );
    let pool_chunks: BTreeSet<ChunkPos> = pool.chunks().collect();
    let liquid_chunks: Vec<ChunkPos> = {
        let world = t.app_mut().world_mut();
        world
            .query::<&LiquidMesh>()
            .iter(world)
            .map(|m| m.pos)
            .filter(|p| pool_chunks.contains(p))
            .collect()
    };
    assert!(!liquid_chunks.is_empty(), "the pool's water is meshed");
    let material = t.screenshot();
    let path = e2e::save_screenshot(&material, "layer_overlay_renders-material", t.frame());
    eprintln!("material: {}", path.display());

    // With keys only: open the editor and step the overlay to the layer.
    let presses = OVERLAY_LAYERS
        .iter()
        .position(|&l| l == LAYER)
        .expect("the picker offers the layer")
        + 1;
    tap(t, TOGGLE_EDITOR_KEY);
    assert!(t.editor().open, "the editor opened");
    let jobs = t.app().world().resource::<MeshStats>().jobs_started;
    let next = key_of(EditorAction::NextOverlay);
    for _ in 0..presses {
        tap(t, next);
    }
    assert_eq!(
        t.editor().overlay,
        Some(LAYER),
        "the picker is on the layer"
    );
    let source = ColorSource::layer(LAYER);
    assert_eq!(
        t.app().world().resource::<MeshColorSource>().0,
        source,
        "the meshes follow the picker"
    );
    // Close the editor so its panel does not cover the frame; the overlay
    // stays.
    tap(t, TOGGLE_EDITOR_KEY);
    assert!(!t.editor().open, "the editor closed");
    settle(t);
    t.steps(2);
    assert_eq!(t.editor().overlay, Some(LAYER), "the overlay stays");
    assert!(
        !t.app().world().resource::<MeshStats>().recoloring,
        "the recolor finished"
    );
    let resident = t.app().world().resource::<Replica>().world().chunk_count() as u64;
    let remeshed = t.app().world().resource::<MeshStats>().jobs_started - jobs;
    eprintln!("remeshed {remeshed} jobs over {resident} resident chunks");
    assert!(
        remeshed >= resident,
        "every resident chunk was meshed again: {remeshed} jobs, {resident} chunks"
    );

    // Semantic stand-in for "mean blue over the pool region higher": the
    // values the overlay reads are higher in the pool than in the ground
    // around it, and the colormap's blue rises with the value.
    let map = Colormap::for_layer(LAYER);
    assert_eq!(source, ColorSource::Layer(LAYER, map));
    assert!(
        map.high[2] > map.low[2],
        "the moisture colormap's blue rises with the value"
    );
    let pool_min = water
        .iter()
        .map(|&c| replica_layer(t, c))
        .min()
        .expect("water");
    let ring_max = ring
        .iter()
        .map(|&c| replica_layer(t, c))
        .max()
        .expect("ground");
    eprintln!("pool moisture >= {pool_min}, ground moisture <= {ring_max}");
    assert!(
        pool_min > ring_max,
        "the pool's moisture ({pool_min}) is above the ground's ({ring_max})"
    );
    assert_eq!(
        map.rgb(pool_min as i16),
        map.high,
        "the full pool takes the colormap's wet end"
    );
    assert!(
        map.rgb(ring_max as i16)[2] < map.rgb(pool_min as i16)[2],
        "the ground's overlay color is less blue than the pool's"
    );

    // The pool's meshes carry the overlay colors, not the material colors.
    for &pos in &liquid_chunks {
        let (opaque, liquid) = colors(t, pos, source);
        let (plain_opaque, plain_liquid) = colors(t, pos, ColorSource::Material);
        assert!(!liquid.is_empty(), "{pos:?}: the overlay keeps the water");
        assert_eq!(
            liquid.len(),
            plain_liquid.len(),
            "{pos:?}: uniform water keeps its geometry"
        );
        assert_ne!(liquid, plain_liquid, "{pos:?}: the water is recolored");
        assert_ne!(opaque, plain_opaque, "{pos:?}: the ground is recolored");
    }

    let overlay = t.screenshot();
    let path = e2e::save_screenshot(&overlay, "layer_overlay_renders", t.frame());
    eprintln!("overlay: {}", path.display());
    assert!(
        material.as_raw() != overlay.as_raw(),
        "the overlay frame differs from the material render"
    );
    assert_eq!(t.render_failure(), None);
}
