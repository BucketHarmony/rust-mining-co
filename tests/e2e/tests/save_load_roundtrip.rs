//! Save and load (SAD §10, §12.4): the player digs a floor cell with the
//! pickaxe and places a 3 × 3 platform with the editor brush, the game
//! saves through its own `SaveRequest`, and a second `TestApp` starts from
//! that save. The loaded world hashes the same as the saved one on host and
//! replica, holds the hole and the platform, and the same view of it looks
//! the same (RMS below a threshold that a missing platform exceeds).

use bevy::prelude::*;
use rmc_client::{ControllerConfig, HudRoot, MeshQueue, MeshStats, Replica};
use rmc_protocol::ClientMsg;
use rmc_testkit::bots::{BuilderBot, DigBot, look_at};
use rmc_testkit::{PlayerSnapshot, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialId, Phase};
use rmc_world::shape::Shape;

const NAME: &str = "save_load_roundtrip";
/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;
/// The player's feet: on the floor, a quarter cell off a cell's middle,
/// facing -Z down the open course.
const FEET: Vec3 = Vec3::new(17.25, FLOOR_M, 24.25);
/// The floor cell the pickaxe digs: beside the player's path, within reach.
const HOLE: CellPos = CellPos(IVec3::new(31, 100, 47));
/// Most normalized RMS (0 to 1, worst RGB channel) between the view before
/// the save and the same view after the load.
const SAME_VIEW_RMS: f64 = 0.01;

fn config() -> TestConfig {
    // 2 × 4 × 2 chunks, full height.
    TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
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

fn replica_world(t: &TestApp) -> &rmc_world::world::World {
    t.app().world().resource::<Replica>().world()
}

/// The world as the camera sees it, with the HUD and its target outline
/// hidden for the capture: the HUD lists the inventory, which a save does
/// not hold (SAD §10), and the outline follows what the pickaxe targets.
fn world_shot(t: &mut TestApp) -> image::RgbaImage {
    set_overlay(t, false);
    let shot = t.screenshot();
    set_overlay(t, true);
    shot
}

fn set_overlay(t: &mut TestApp, shown: bool) {
    let world = t.app_mut().world_mut();
    let mut roots = world.query_filtered::<&mut Visibility, With<HudRoot>>();
    for mut v in roots.iter_mut(world) {
        *v = if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
    let mut gizmos = world.resource_mut::<GizmoConfigStore>();
    gizmos.config_mut::<DefaultGizmoConfigGroup>().0.enabled = shown;
}

/// Turns a player standing still at yaw 0, pitch 0 to `view`'s yaw and
/// pitch with mouse motion.
fn turn_to(t: &mut TestApp, view: &PlayerSnapshot) {
    let p = t.player();
    let sensitivity = t
        .app()
        .world()
        .resource::<ControllerConfig>()
        .sensitivity_rad;
    t.look(
        (p.yaw - view.yaw) / sensitivity,
        (p.pitch - view.pitch) / sensitivity,
    );
    t.step();
}

#[test]
#[ignore]
fn save_load_roundtrip() {
    let dir = e2e::out_dir().join(format!("{NAME}-save"));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("remove the last run's save");
    }
    let mut t = TestApp::new(config());
    let mut saved = None;
    e2e::run_scenario(&mut t, NAME, |t| saved = Some(dig_place_save(t, &dir)));
    let saved = saved.expect("the first half ran");

    let mut loaded = TestApp::load(&dir, config());
    e2e::run_scenario(&mut loaded, &format!("{NAME}-loaded"), |t| {
        check_loaded(t, &saved)
    });
}

/// What the first app saved, for the loaded one to match.
struct Saved {
    hash: u64,
    platform: Vec<CellPos>,
    brass: MaterialId,
    view: PlayerSnapshot,
    shot: image::RgbaImage,
}

fn dig_place_save(t: &mut TestApp, dir: &std::path::Path) -> Saved {
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
    settle(t);

    // Dig: the pickaxe breaks a floor cell beside the path.
    let held = t.inventory().items().iter().map(|&(_, n)| n).sum::<u32>();
    let dug = DigBot::new(HOLE)
        .run(t)
        .unwrap_or_else(|e| panic!("DigBot: {e}"));
    eprintln!("dug {HOLE:?} in {} swings", dug.swings);
    t.steps(2);
    assert_eq!(replica_world(t).get_cell(HOLE), Some(MaterialId::AIR));
    assert_eq!(t.host_world().get_cell(HOLE), Some(MaterialId::AIR));
    let now = t.inventory().items().iter().map(|&(_, n)| n).sum::<u32>();
    assert!(now > held, "the dig yielded something: {held} -> {now}");

    // Place: facing down the course again, BuilderBot lays a 3 × 3
    // platform ahead with the brush and climbs it.
    let ahead = t.player().eye + Vec3::NEG_Z * 10.0;
    look_at(t, ahead).unwrap_or_else(|e| panic!("look down the course: {e}"));
    let built = BuilderBot::new(3)
        .run(t)
        .unwrap_or_else(|e| panic!("BuilderBot: {e}"));
    assert_eq!(built.cells.len(), 9);
    for &c in &built.cells {
        assert_eq!(t.host_world().get_cell(c), Some(built.material), "{c:?}");
    }

    // The view to compare: from the platform, down at the hole, with the
    // platform's edge in sight.
    look_at(t, HOLE.center_m()).unwrap_or_else(|e| panic!("look at the hole: {e}"));
    t.steps(10);
    settle(t);
    let view = t.player();
    assert!(view.grounded, "standing on the platform: {view:?}");
    let path = e2e::save_screenshot(&t.screenshot(), NAME, t.frame());
    eprintln!("with the HUD: {}", path.display());
    let shot = world_shot(t);
    let path = e2e::save_screenshot(&shot, &format!("{NAME}-world"), t.frame());
    eprintln!("before the save: {}", path.display());

    // Save through the game's own SaveRequest.
    let outcome = t.save(dir);
    eprintln!("saved: {outcome:?}");
    assert!(outcome.ok, "the host saved: {outcome:?}");
    assert_eq!(std::path::Path::new(&outcome.path), dir);
    assert!(dir.join("header.ron").is_file() && dir.join("chunks.bin").is_file());
    let hash = t.host_world().hash();
    assert_eq!(
        replica_world(t).hash(),
        hash,
        "host and replica at the save"
    );

    Saved {
        hash,
        platform: built.cells,
        brass: built.material,
        view,
        shot,
    }
}

fn check_loaded(t: &mut TestApp, saved: &Saved) {
    // The loaded host is the saved world, and the replica joins it.
    assert_eq!(t.host_world().hash(), saved.hash, "loaded host hash");
    settle(t);
    assert_eq!(replica_world(t).hash(), saved.hash, "loaded replica hash");
    for world in [t.host_world(), replica_world(t)] {
        assert_eq!(world.get_cell(HOLE), Some(MaterialId::AIR), "the hole");
        for &c in &saved.platform {
            assert_eq!(world.get_cell(c), Some(saved.brass), "platform {c:?}");
        }
    }

    // The same view: the player where it stood at the save, turned the
    // same way.
    t.spawn_player(saved.view.pos);
    t.step();
    turn_to(t, &saved.view);
    t.steps(10);
    settle(t);
    let view = t.player();
    assert!(view.grounded, "standing on the loaded platform: {view:?}");
    assert!(
        view.pos.distance(saved.view.pos) < 1e-3
            && (view.yaw - saved.view.yaw).abs() < 1e-4
            && (view.pitch - saved.view.pitch).abs() < 1e-4,
        "same pose: {view:?} vs {:?}",
        saved.view
    );
    let shot = world_shot(t);
    let path = e2e::save_screenshot(&shot, &format!("{NAME}-loaded-world"), t.frame());
    eprintln!("after the load: {}", path.display());
    let rms = e2e::rms(&saved.shot, &shot);
    eprintln!("RMS before save vs after load: {rms:.5}");
    assert!(rms < SAME_VIEW_RMS, "the loaded view differs: RMS {rms:.5}");

    // The threshold tells worlds apart: with the hole filled again the
    // same view is off by more.
    let floor = t
        .host_world()
        .get_cell(CellPos(HOLE.0 + IVec3::X))
        .expect("the floor beside the hole");
    edit_boxes(t, &[(EditOp::Set(floor), HOLE, HOLE)]);
    assert_eq!(replica_world(t).get_cell(HOLE), Some(floor));
    settle(t);
    let filled = world_shot(t);
    let path = e2e::save_screenshot(&filled, &format!("{NAME}-control"), t.frame());
    eprintln!("control with the hole filled: {}", path.display());
    let control = e2e::rms(&saved.shot, &filled);
    eprintln!("RMS before save vs control: {control:.5}");
    let path = e2e::save_screenshot(&t.screenshot(), &format!("{NAME}-loaded"), t.frame());
    eprintln!("loaded, with the HUD: {}", path.display());
    assert!(
        control > SAME_VIEW_RMS,
        "the threshold does not see the filled hole: RMS {control:.5}"
    );
    assert_eq!(t.render_failure(), None);
}
