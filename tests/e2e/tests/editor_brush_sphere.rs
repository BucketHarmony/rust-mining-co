//! The editor's sphere brush (SAD §11.6, §12.4): with keys only, the editor
//! opens (F1), picks Remove (2), the Sphere shape (6) and radius 4 (`]`
//! three times), and one click at a stone block's face clears exactly the
//! block's cells inside the radius-4 sphere around the hit cell, on the
//! host and the replica. The edit remeshes at least one chunk, and the
//! host and replica worlds hash the same.

use bevy::prelude::*;
use rmc_client::{BRUSH_BUTTON, BrushOp, BrushShape, EditorGizmos, MeshQueue, MeshStats, Replica};
use rmc_protocol::ClientMsg;
use rmc_testkit::bots::look_at;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::shape::Shape;

/// The brush radius, in cells.
const RADIUS: u16 = 4;
/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;
/// The stone block: its near (+Z) face at z = 18 m.
const BLOCK_MIN: CellPos = CellPos::new(26, 101, 20);
const BLOCK_MAX: CellPos = CellPos::new(42, 112, 35);
/// The cell on the block's near face the brush aims at.
const AIM: CellPos = CellPos::new(34, 106, 35);
/// The player's feet: on the floor, 3 m in front of the block's face.
const FEET: Vec3 = Vec3::new(17.25, FLOOR_M, 21.25);

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

fn edit_boxes(t: &mut TestApp, edits: &[(EditOp, CellPos, CellPos)]) {
    for (seq, &(op, min, max)) in (1000..).zip(edits) {
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
}

/// Presses and releases `key`, one step each.
fn tap(t: &mut TestApp, key: KeyCode) {
    t.press(key);
    t.step();
    t.release(key);
    t.step();
}

fn replica_cell(t: &TestApp, cell: CellPos) -> Option<MaterialId> {
    t.app().world().resource::<Replica>().world().get_cell(cell)
}

fn replica_hash(t: &TestApp) -> u64 {
    t.app().world().resource::<Replica>().world().hash()
}

fn block_cells() -> impl Iterator<Item = CellPos> {
    (BLOCK_MIN.0.x..=BLOCK_MAX.0.x).flat_map(|x| {
        (BLOCK_MIN.0.y..=BLOCK_MAX.0.y)
            .flat_map(move |y| (BLOCK_MIN.0.z..=BLOCK_MAX.0.z).map(move |z| CellPos::new(x, y, z)))
    })
}

/// Solid cells of the host world within `r` cells (on every axis) of the
/// sphere's center: the sphere's box and a margin.
fn solid_near(t: &TestApp, center: CellPos, r: i32) -> usize {
    let host = t.host_world();
    (-r..=r)
        .flat_map(|x| (-r..=r).flat_map(move |y| (-r..=r).map(move |z| IVec3::new(x, y, z))))
        .filter(|&d| host.is_solid(CellPos(center.0 + d)))
        .count()
}

#[test]
#[ignore]
fn editor_brush_sphere() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "editor_brush_sphere", brush);
}

fn brush(t: &mut TestApp) {
    settle(t);

    // The course, high above the terrain: a floor, everything above it
    // cleared, and a stone block across the view.
    let stone = t
        .host_world()
        .materials()
        .iter()
        .find(|d| d.phase == Phase::Solid && d.emissive == 0.0 && d.diggable)
        .expect("assets/materials.ron has a plain diggable solid")
        .id;
    edit_boxes(
        t,
        &[
            (
                EditOp::Set(stone),
                CellPos::new(20, 100, 4),
                CellPos::new(43, 100, 59),
            ),
            (
                EditOp::Remove,
                CellPos::new(20, 101, 4),
                CellPos::new(43, 124, 59),
            ),
            (EditOp::Set(stone), BLOCK_MIN, BLOCK_MAX),
        ],
    );
    assert!(block_cells().all(|c| t.host_world().get_cell(c) == Some(stone)));
    t.spawn_player(FEET);
    t.steps(30);
    assert!(t.player().grounded, "landed on the floor: {:?}", t.player());
    settle(t);

    // Keys only: open, Remove, Sphere, radius 1 -> 4, targeted-cell gizmo.
    tap(t, KeyCode::F1);
    tap(t, KeyCode::Digit2);
    tap(t, KeyCode::Digit6);
    for _ in 1..RADIUS {
        tap(t, KeyCode::BracketRight);
    }
    tap(t, KeyCode::F3);
    let editor = t.editor().clone();
    assert!(editor.open, "F1 opened the editor");
    assert_eq!(
        (editor.op, editor.shape, editor.size),
        (BrushOp::Remove, BrushShape::Sphere, RADIUS)
    );
    assert!(editor.gizmos.target_cell);

    // Aim at the middle of AIM's near face. The gizmo shows the cell the
    // sphere centers on: the hit cell itself for Remove.
    let face = AIM.center_m() + Vec3::Z * (CELL_SIZE_M / 2.0);
    look_at(t, face).unwrap_or_else(|e| panic!("aiming: {e}"));
    let target = t
        .app()
        .world()
        .resource::<EditorGizmos>()
        .target
        .expect("the brush targets the block");
    assert_eq!(
        (target.cell, target.face_normal, target.brush),
        (AIM, IVec3::Z, Some(AIM)),
        "the brush targets AIM's near face"
    );

    // What the sphere should clear: its cells that are solid now. Half the
    // sphere sticks out of the block's face into the air.
    let sphere = Shape::Sphere {
        center: AIM,
        radius: RADIUS,
    };
    let expected: Vec<CellPos> = sphere
        .cells()
        .filter(|&c| t.host_world().is_solid(c))
        .collect();
    let in_block = sphere
        .cells()
        .filter(|&c| block_cells().any(|b| b == c))
        .count();
    assert_eq!(
        expected.len(),
        in_block,
        "the sphere's solid cells are the block's"
    );
    // The same cells counted by hand, not through Shape: every offset
    // within RADIUS of AIM on the block's side of its face (dz <= 0). The
    // block reaches past the sphere on every other side.
    let r = i32::from(RADIUS);
    let by_hand: Vec<CellPos> = (-r..=r)
        .flat_map(|x| (-r..=r).flat_map(move |y| (-r..=r).map(move |z| IVec3::new(x, y, z))))
        .filter(|d| d.length_squared() <= r * r && d.z <= 0)
        .map(|d| CellPos(AIM.0 + d))
        .collect();
    let mut sorted = expected.clone();
    sorted.sort_by_key(|c| c.0.to_array());
    let mut by_hand_sorted = by_hand.clone();
    by_hand_sorted.sort_by_key(|c| c.0.to_array());
    assert_eq!(sorted, by_hand_sorted, "the radius-4 half sphere");
    assert_eq!(
        expected.len(),
        153,
        "radius 4: 257 cells, 153 on the block's side"
    );
    assert_eq!(sphere.cell_count(), 257);
    let margin = i32::from(RADIUS) + 2;
    let solid_before = solid_near(t, AIM, margin);
    let before = t.screenshot();
    e2e::save_screenshot(&before, "editor_brush_sphere", t.frame());

    let stats = t.app().world().resource::<MeshStats>().clone();
    let block_before = block_cells()
        .filter(|&c| t.host_world().get_cell(c) == Some(stone))
        .count();
    t.click(BRUSH_BUTTON);
    t.steps(3);
    settle(t);

    // Exactly the sphere's solid cells are gone, on the host and replica.
    let solid_after = solid_near(t, AIM, margin);
    eprintln!(
        "sphere of {} cells cleared {} (solid near it {} -> {})",
        sphere.cell_count(),
        expected.len(),
        solid_before,
        solid_after
    );
    assert_eq!(
        solid_before - solid_after,
        expected.len(),
        "the cell count change"
    );
    for &c in &expected {
        assert_eq!(
            t.host_world().get_cell(c),
            Some(MaterialId::AIR),
            "host {c:?}"
        );
        assert_eq!(replica_cell(t, c), Some(MaterialId::AIR), "replica {c:?}");
    }
    let block_after = block_cells()
        .filter(|&c| t.host_world().get_cell(c) == Some(stone))
        .count();
    assert_eq!(
        block_before - block_after,
        expected.len(),
        "only the sphere"
    );

    // The edit remeshed at least one chunk.
    let after_stats = t.app().world().resource::<MeshStats>().clone();
    let (jobs, uploads) = (
        after_stats.jobs_started - stats.jobs_started,
        after_stats.uploads - stats.uploads,
    );
    eprintln!("remesh: {jobs} jobs, {uploads} uploads");
    assert!(jobs >= 1 && uploads >= 1, "at least one remesh");

    t.step();
    assert_eq!(
        replica_hash(t),
        t.host_world().hash(),
        "host and replica hashes"
    );
    let after = t.screenshot();
    let path = e2e::save_screenshot(&after, "editor_brush_sphere", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None);
}
