//! Building and climbing (SAD §12.3, §12.4): BuilderBot opens the editor
//! with F1, picks Set and the Cell shape with keys, places a 3 × 3 platform
//! of the brush's default `placeable` material (brass) one click per cell,
//! closes the editor and walks onto the platform. The player ends standing
//! on it, every platform cell is on the host and the replica, and the host
//! and replica worlds hash the same.

use bevy::prelude::*;
use rmc_client::{BrushOp, BrushShape, MeshQueue, MeshStats, PLAYER_WIDTH_M, Replica};
use rmc_protocol::ClientMsg;
use rmc_testkit::bots::BuilderBot;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::shape::Shape;

/// Cells on a side of the platform.
const SIDE: i32 = 3;
/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;
/// Top of the platform (one cell on the floor), in meters.
const PLATFORM_M: f32 = 102.0 * CELL_SIZE_M;
/// The player's feet: on the floor, a quarter cell off a cell's middle,
/// facing -Z down the open course.
const FEET: Vec3 = Vec3::new(17.25, FLOOR_M, 24.25);

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

fn replica_cell(t: &TestApp, cell: CellPos) -> Option<MaterialId> {
    t.app().world().resource::<Replica>().world().get_cell(cell)
}

fn replica_hash(t: &TestApp) -> u64 {
    t.app().world().resource::<Replica>().world().hash()
}

#[test]
#[ignore]
fn place_and_climb() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "place_and_climb", build);
}

fn build(t: &mut TestApp) {
    settle(t);

    // The course, high above the terrain: a stone floor with its top at
    // FLOOR_M and everything above it cleared.
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
                EditOp::Remove,
                CellPos::new(20, 101, 4),
                CellPos::new(43, 124, 59),
            ),
            (
                EditOp::Set(stone),
                CellPos::new(20, 100, 4),
                CellPos::new(43, 100, 59),
            ),
        ],
    );
    t.spawn_player(FEET);
    t.steps(30);
    assert!(t.player().grounded, "landed on the floor: {:?}", t.player());
    assert!(
        (t.player().pos.y - FLOOR_M).abs() < 0.01,
        "{:?}",
        t.player()
    );
    settle(t);
    assert!(!t.editor().open, "the editor starts closed");

    let report = BuilderBot::new(SIDE)
        .run(t)
        .unwrap_or_else(|e| panic!("BuilderBot: {e}"));
    eprintln!(
        "placed {} cells in {} frames; feet {:?}",
        report.cells.len(),
        report.frames,
        t.player().pos
    );

    // The bot drove the editor with keys: Set, Cell, and closed it again.
    let editor = t.editor().clone();
    assert_eq!((editor.op, editor.shape), (BrushOp::Set, BrushShape::Cell));
    assert!(!editor.open, "the bot closed the editor");

    // The platform: SIDE × SIDE cells of the brush's default material, one
    // cell high on the floor, ahead of where the player started.
    let materials = t.host_world().materials().clone();
    let brass = report.material;
    let def = materials.get(brass).expect("a known material");
    eprintln!("platform of {}", def.key);
    assert_eq!(def.key, "brass_plate", "the default brush material");
    assert!(materials.has_tag(brass, "placeable") && def.is_solid());
    assert_eq!(report.cells.len(), (SIDE * SIDE) as usize);
    let mut cells = report.cells.clone();
    cells.sort_by_key(|c| c.0.to_array());
    cells.dedup();
    assert_eq!(cells.len(), (SIDE * SIDE) as usize, "nine distinct cells");
    let (min, max) = cells.iter().fold((IVec3::MAX, IVec3::MIN), |(lo, hi), c| {
        (lo.min(c.0), hi.max(c.0))
    });
    assert_eq!(max - min, IVec3::new(SIDE - 1, 0, SIDE - 1), "a 3 × 3 slab");
    assert_eq!(min.y, 101, "standing on the floor");
    assert!(
        (max.z + 1) as f32 * CELL_SIZE_M < FEET.z - PLAYER_WIDTH_M / 2.0,
        "ahead of the start: {min} to {max}"
    );
    for &c in &cells {
        assert_eq!(t.host_world().get_cell(c), Some(brass), "host {c:?}");
        assert_eq!(replica_cell(t, c), Some(brass), "replica {c:?}");
        assert_eq!(
            t.host_world().get_cell(CellPos(c.0 - IVec3::Y)),
            Some(stone),
            "floor under {c:?}"
        );
    }
    // Nothing else was placed around it.
    for x in min.x - 1..=max.x + 1 {
        for z in min.z - 1..=max.z + 1 {
            for y in 101..=103 {
                let c = CellPos::new(x, y, z);
                if !cells.contains(&c) {
                    assert_eq!(t.host_world().get_cell(c), Some(MaterialId::AIR), "{c:?}");
                }
            }
        }
    }

    // The player stands on the platform: grounded at its top, the box's
    // footprint inside the platform's.
    t.steps(10);
    let p = t.player();
    eprintln!("on the platform: {p:?}");
    assert!(p.grounded, "standing: {p:?}");
    assert!((p.pos.y - PLATFORM_M).abs() < 0.01, "on top: {p:?}");
    let half = PLAYER_WIDTH_M / 2.0;
    let (lo, hi) = (
        min.as_vec3() * CELL_SIZE_M,
        (max + IVec3::ONE).as_vec3() * CELL_SIZE_M,
    );
    for axis in [0, 2] {
        assert!(
            p.pos[axis] - half >= lo[axis] && p.pos[axis] + half <= hi[axis],
            "the box is over the platform on axis {axis}: {p:?}, platform {lo} to {hi}"
        );
    }
    assert!(t.player_penetration().is_empty());

    // The host has every edit and agrees with the replica on every cell.
    t.step();
    assert_eq!(
        replica_hash(t),
        t.host_world().hash(),
        "host and replica hashes"
    );

    // The view: turned back toward the start, looking down past the
    // platform's edge to the floor.
    let sensitivity = t
        .app()
        .world()
        .resource::<rmc_client::ControllerConfig>()
        .sensitivity_rad;
    t.look(
        std::f32::consts::PI / sensitivity,
        60f32.to_radians() / sensitivity,
    );
    t.step();
    settle(t);
    let shot = t.screenshot();
    let path = e2e::save_screenshot(&shot, "place_and_climb", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None);
}
