//! Digging, carrying and placing (owner extension F093, SAD §8.1, §11.5):
//! the player digs three topsoil cells out of a row straight ahead with the
//! pickaxe, selects topsoil with the mouse wheel and puts the three cells
//! back with right clicks. The host world has them again, the topsoil count
//! is back where it started, and a fourth right click, with no topsoil
//! left, is rejected by the host and changes nothing.

use bevy::prelude::*;
use rmc_client::{FromHost, MeshQueue, MeshStats, NetSystems, PLACE_BUTTON, Replica, SWING_BUTTON};
use rmc_protocol::{ClientMsg, ServerMsg};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, RejectReason};
use rmc_world::materials::MaterialId;
use rmc_world::shape::Shape;

/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;
/// The player's feet: on the floor in the middle of cell column x = 34,
/// z = 40, facing -Z. The eye (1.55 m up, 52.05 m) is in cell row y = 104.
const FEET: Vec3 = Vec3::new(17.25, FLOOR_M, 20.25);
/// The topsoil row straight ahead of the eye, nearest first: its near face
/// is 0.75 m from the eye.
const ROW: [CellPos; 3] = [
    CellPos::new(34, 104, 38),
    CellPos::new(34, 104, 37),
    CellPos::new(34, 104, 36),
];
/// The stone cell behind the row, 2.25 m from the eye: what the pickaxe
/// looks at once the row is dug, and what the first cell is placed against.
const BACKSTOP: CellPos = CellPos::new(34, 104, 35);
/// Frames allowed to break one cell or to see one place arrive.
const MAX_FRAMES: u32 = 600;
/// Frames between right clicks: more than the swing interval (21 steps).
const CLICK_GAP: u32 = 30;

/// Every `Applied` result the client received, in order.
#[derive(Resource, Default)]
struct Results(Vec<(EditStatus, CommandId)>);

fn record_results(mut inbox: MessageReader<FromHost>, mut results: ResMut<Results>) {
    for FromHost(msg) in inbox.read() {
        if let ServerMsg::Applied(r) = msg {
            results.0.push((r.status.clone(), r.command));
        }
    }
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

/// Steps until `done`, at most [`MAX_FRAMES`] times.
fn step_until(t: &mut TestApp, what: &str, done: impl Fn(&TestApp) -> bool) {
    for _ in 0..MAX_FRAMES {
        if done(t) {
            return;
        }
        t.step();
    }
    panic!("{what}: not within {MAX_FRAMES} frames");
}

#[test]
#[ignore]
fn dig_carry_place() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        size: (320, 180),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "dig_carry_place", play);
}

fn play(t: &mut TestApp) {
    settle(t);
    t.app_mut()
        .init_resource::<Results>()
        .add_systems(PreUpdate, record_results.after(NetSystems::Receive));
    let materials = t.host_world().materials().clone();
    let topsoil = materials.id("topsoil").expect("topsoil in the table");
    let stone = materials.id("stone").expect("stone in the table");

    // The course, high above the terrain: a stone floor, everything above
    // it cleared, the topsoil row and the stone behind it.
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
            (EditOp::Set(topsoil), ROW[2], ROW[0]),
            (EditOp::Set(stone), BACKSTOP, BACKSTOP),
        ],
    );
    t.spawn_player(FEET);
    t.steps(30);
    assert!(t.player().grounded, "landed on the floor: {:?}", t.player());
    settle(t);
    assert_eq!(t.hud().target, Some(ROW[0]), "looking at the row");
    let start = t.inventory().count(topsoil);

    // Dig the row with the pickaxe, one cell at a time.
    for (i, &cell) in ROW.iter().enumerate() {
        t.mouse_down(SWING_BUTTON);
        let want = start + i as u32 + 1;
        step_until(t, "dig a topsoil cell", |t| {
            t.inventory().count(topsoil) == want
        });
        t.mouse_up(SWING_BUTTON);
        t.steps(2);
        assert_eq!(t.host_world().get_cell(cell), Some(MaterialId::AIR));
        assert_eq!(replica_cell(t, cell), Some(MaterialId::AIR));
    }
    assert_eq!(t.inventory().count(topsoil), start + 3);
    assert_eq!(t.host_world().get_cell(BACKSTOP), Some(stone));
    assert_eq!(t.hud().target, Some(BACKSTOP), "the stone behind shows");
    eprintln!("dug {} cells; inventory {:?}", ROW.len(), t.inventory());

    // Select topsoil with the wheel: the only held material.
    assert_eq!(t.hud().selected, None, "nothing selected at first");
    t.scroll(-1.0);
    t.step();
    assert_eq!(t.hud().selected, Some(topsoil));
    assert!(
        t.hud()
            .inventory
            .iter()
            .any(|item| item.material == topsoil && item.count == start + 3)
    );

    // Three right clicks put the row back, farthest cell first: each lands
    // against the face of the cell placed before it.
    for (i, &cell) in ROW.iter().rev().enumerate() {
        t.click(PLACE_BUTTON);
        let want = start + 2 - i as u32;
        step_until(t, "place a topsoil cell", |t| {
            t.host_world().get_cell(cell) == Some(topsoil) && t.inventory().count(topsoil) == want
        });
        t.steps(CLICK_GAP);
        assert_eq!(replica_cell(t, cell), Some(topsoil));
    }
    for &cell in &ROW {
        assert_eq!(t.host_world().get_cell(cell), Some(topsoil), "{cell:?}");
    }
    assert_eq!(t.inventory().count(topsoil), start, "back to the start");
    assert_eq!(t.hud().target, Some(ROW[0]));
    assert_eq!(replica_hash(t), t.host_world().hash());
    let placed: Vec<_> = t.app().world().resource::<Results>().0.clone();
    assert!(
        placed
            .iter()
            .all(|(status, _)| *status == EditStatus::Applied),
        "{placed:?}"
    );

    // A fourth right click: the selection stays, but there is no topsoil
    // left. The host rejects it, and nothing changes.
    assert_eq!(start, 0, "the player started with no topsoil");
    assert_eq!(t.hud().selected, Some(topsoil));
    let before = t.host_world().hash();
    let seen = placed.len();
    t.click(PLACE_BUTTON);
    step_until(t, "the rejection", |t| {
        t.app().world().resource::<Results>().0.len() > seen
    });
    t.steps(CLICK_GAP);
    let results = &t.app().world().resource::<Results>().0;
    assert_eq!(results.len(), seen + 1, "{results:?}");
    assert_eq!(
        results[seen].0,
        EditStatus::Rejected(RejectReason::NotInInventory(topsoil))
    );
    assert_eq!(
        t.host_world().hash(),
        before,
        "the rejected place changed nothing"
    );
    assert_eq!(replica_hash(t), before);
    assert_eq!(t.inventory().count(topsoil), 0);
    assert!(
        t.player_penetration().is_empty(),
        "{:?}",
        t.player_penetration()
    );
    let shot = t.screenshot();
    let path = e2e::save_screenshot(&shot, "dig_carry_place", t.frame());
    eprintln!("after: {}", path.display());
}
