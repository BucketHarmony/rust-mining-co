//! Dig latency (SAD §12.4, §13): with the host ticked synchronously, the
//! swing that breaks a cell is visible at most 2 steps after the step that
//! sent it. Visible means the replica holds the broken cell and the cell's
//! chunk has an uploaded mesh built from that revision, so the frame
//! rendered in that step draws the hole.

use bevy::prelude::*;
use rmc_client::{MeshQueue, MeshStats, PickTarget, Pickaxe, Replica, SWING_BUTTON, SwingState};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::shape::Shape;

/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;
/// The wall cell the player digs: interior to its chunk on every axis, so
/// breaking it changes that chunk's mesh only.
const TARGET: CellPos = CellPos::new(34, 104, 36);
/// The player's feet: centered on the target's column, 1.75 m from the
/// wall's near face (inside the pickaxe's reach).
const FEET: Vec3 = Vec3::new(17.25, FLOOR_M, 20.25);
/// Most steps from the breaking swing to the visible change (SAD §12.4).
const MAX_LATENCY_STEPS: u32 = 2;

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

/// What one step left behind, for the latency measurement.
#[derive(Clone, Copy, Debug)]
struct Sample {
    frame: u32,
    swings: u64,
    replica_cell: Option<MaterialId>,
    replica_revision: Option<u64>,
    meshed_revision: Option<u64>,
}

impl Sample {
    fn take(t: &TestApp) -> Self {
        let world = t.app().world();
        let replica = world.resource::<Replica>().world();
        let chunk = TARGET.chunk();
        Self {
            frame: t.frame(),
            swings: world.resource::<SwingState>().swings(),
            replica_cell: replica.get_cell(TARGET),
            replica_revision: replica.chunk(chunk).map(|c| c.revision),
            meshed_revision: world.resource::<MeshQueue>().meshed_revision(chunk),
        }
    }

    /// The hole is in the replica and in the chunk's uploaded mesh.
    fn visible(&self) -> bool {
        self.replica_cell == Some(MaterialId::AIR)
            && self.replica_revision.is_some()
            && self.meshed_revision == self.replica_revision
    }
}

#[test]
#[ignore]
fn latency_dig() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "latency_dig", measure);
}

fn measure(t: &mut TestApp) {
    settle(t);

    // A plain solid that takes more than one swing, so the swings before
    // the breaking one are seen to change nothing visible.
    let profile = t.app().world().resource::<Pickaxe>().0;
    let wall = t
        .host_world()
        .materials()
        .iter()
        .find(|d| {
            d.phase == Phase::Solid
                && d.emissive == 0.0
                && d.diggable
                && d.hardness > profile.damage
        })
        .expect("assets/materials.ron has a plain solid that takes several swings")
        .clone();

    // A floor with its top at FLOOR_M, everything above it cleared, and a
    // one-cell-thick wall across the view with TARGET in it.
    edit_boxes(
        t,
        &[
            (
                EditOp::Set(wall.id),
                CellPos::new(20, 100, 4),
                CellPos::new(43, 100, 59),
            ),
            (
                EditOp::Remove,
                CellPos::new(20, 101, 4),
                CellPos::new(43, 124, 59),
            ),
            (
                EditOp::Set(wall.id),
                CellPos::new(26, 101, 36),
                CellPos::new(42, 112, 36),
            ),
        ],
    );
    t.spawn_player(FEET);
    t.steps(30);
    assert!(t.player().grounded, "landed on the floor: {:?}", t.player());
    settle(t);
    let target = t.app().world().resource::<PickTarget>().0.map(|h| h.cell);
    assert_eq!(target, Some(TARGET), "the player looks at the target");

    let start = Sample::take(t);
    assert!(
        start.replica_revision.is_some() && start.meshed_revision == start.replica_revision,
        "the target's chunk is meshed before the dig: {start:?}"
    );
    assert_eq!(start.swings, 0, "no swing before the dig");

    // Hold the swing button and record every step until the hole is
    // visible.
    let swings = u64::from(wall.hardness.div_ceil(profile.damage));
    let interval = (profile.interval_s * 60.0).ceil() as u32;
    let limit = swings as u32 * interval + 10;
    t.mouse_down(SWING_BUTTON);
    let mut samples = Vec::new();
    loop {
        t.step();
        let s = Sample::take(t);
        samples.push(s);
        if s.visible() {
            break;
        }
        assert!(
            samples.len() < limit as usize,
            "the hole became visible within {limit} steps: {samples:#?}"
        );
    }
    t.mouse_up(SWING_BUTTON);

    let sent = samples
        .iter()
        .find(|s| s.swings == swings)
        .expect("the breaking swing went out");
    let applied = samples
        .iter()
        .find(|s| s.replica_cell == Some(MaterialId::AIR))
        .expect("the replica applied the break");
    let visible = samples.last().expect("at least one step");
    eprintln!(
        "breaking swing in frame {}, replica delta in frame {}, mesh uploaded in frame {}",
        sent.frame, applied.frame, visible.frame
    );

    // The swings before the breaking one changed nothing the client sees.
    assert!(
        samples
            .iter()
            .filter(|s| s.frame < sent.frame)
            .all(|s| s.replica_cell == Some(wall.id)
                && s.replica_revision == start.replica_revision
                && s.meshed_revision == start.meshed_revision),
        "only the breaking swing changes the chunk: {samples:#?}"
    );
    assert!(sent.frame <= applied.frame && applied.frame <= visible.frame);
    let latency = visible.frame - sent.frame;
    assert!(
        latency <= MAX_LATENCY_STEPS,
        "the break was visible {latency} steps after the swing (at most {MAX_LATENCY_STEPS}): {samples:#?}"
    );
    assert_eq!(t.host_world().get_cell(TARGET), Some(MaterialId::AIR));

    settle(t);
    let shot = t.screenshot();
    let path = e2e::save_screenshot(&shot, "latency_dig", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert_eq!(t.render_failure(), None);
}
