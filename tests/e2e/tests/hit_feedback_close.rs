//! Hit feedback at close range (owner change F096, SAD §11.5): the player
//! swings at a basalt wall from 0.4 m and from 1.5 m, and from 0.25 m,
//! about as close as the 0.45 m box stands (the pt-725 view).
//!
//! Mid-damage, the targeted cell carries a crack overlay whose strength is
//! the HUD's damage fraction. On the break, the chips are solid cube meshes
//! in a material (no gizmo wireframes), the overlay is gone, and the chips
//! are despawned when the effect ends. The outline and flash gizmo group
//! has no depth bias. Each phase is screenshotted for the playtester
//! (CLAUDE.md rule 21: no pixel thresholds; a frame only has to be not
//! blank).

use std::collections::BTreeSet;

use bevy::prelude::*;
use image::RgbaImage;
use rmc_client::{
    BREAK_CHIPS, BreakChip, CrackOverlay, HUD_GIZMO_DEPTH_BIAS, HudGizmos, MeshQueue, MeshStats,
    Player, Replica, SWING_BUTTON, crack_color,
};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, bots, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialDef, MaterialId};
use rmc_world::shape::Shape;

/// Top cell row of the floor; the feet stand on its top face.
const FLOOR_ROW: i32 = 100;
const FLOOR_M: f32 = (FLOOR_ROW + 1) as f32 * CELL_SIZE_M;
/// The row of the targeted cells: the eye's (1.55 m above the floor).
const EYE_ROW: i32 = 104;
/// The wall's near cell row and its near face (toward +Z), in meters.
const WALL_NEAR_Z: i32 = 36;
const WALL_FACE_M: f32 = (WALL_NEAR_Z + 1) as f32 * CELL_SIZE_M;
/// The close, reading and touching distance from the face, and the cell
/// column the player swings at from each.
const SHOTS: [(&str, f32, i32); 3] = [
    ("close", 0.4, 28),
    ("reading", 1.5, 38),
    ("touching", 0.25, 33),
];
/// The overlay must show by this damage fraction.
const MID_FRACTION: f32 = 0.5;
/// Frames allowed to reach a damage stage or a break.
const MAX_FRAMES: u32 = 600;
/// A frame with fewer distinct colors than this is blank (a flat fill).
const MIN_COLORS: usize = 16;

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

fn material(t: &TestApp, key: &str) -> MaterialDef {
    t.host_world()
        .materials()
        .iter()
        .find(|d| d.key == key)
        .unwrap_or_else(|| panic!("assets/materials.ron has {key}"))
        .clone()
}

fn replica_cell(t: &TestApp, cell: CellPos) -> Option<MaterialId> {
    t.app().world().resource::<Replica>().world().get_cell(cell)
}

fn distinct_colors(image: &RgbaImage) -> usize {
    image
        .pixels()
        .map(|p| [p.0[0], p.0[1], p.0[2]])
        .collect::<BTreeSet<_>>()
        .len()
}

/// Screenshots the view as `hit_feedback_close-<label>` and checks it is
/// not blank.
fn shoot(t: &mut TestApp, label: &str) {
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("hit_feedback_close-{label}"), t.frame());
    assert_eq!(t.render_failure(), None);
    let colors = distinct_colors(&image);
    eprintln!("{label}: {} ({colors} colors)", path.display());
    assert!(
        colors >= MIN_COLORS,
        "{label}: a blank frame ({colors} colors)"
    );
}

/// The crack overlays: component and material color.
fn cracks(t: &mut TestApp) -> Vec<(CrackOverlay, Color)> {
    let world = t.app_mut().world_mut();
    let found: Vec<_> = world
        .query::<(&CrackOverlay, &MeshMaterial3d<StandardMaterial>)>()
        .iter(world)
        .map(|(c, m)| (*c, m.0.clone()))
        .collect();
    let materials = world.resource::<Assets<StandardMaterial>>();
    found
        .into_iter()
        .map(|(c, m)| (c, materials.get(&m).expect("overlay material").base_color))
        .collect()
}

/// The chips: those with a mesh and a material, and all of them.
fn chips(t: &mut TestApp) -> (usize, usize) {
    let world = t.app_mut().world_mut();
    let meshed = world
        .query_filtered::<(), (
            With<BreakChip>,
            With<Mesh3d>,
            With<MeshMaterial3d<StandardMaterial>>,
        )>()
        .iter(world)
        .count();
    let all = world
        .query_filtered::<(), With<BreakChip>>()
        .iter(world)
        .count();
    (meshed, all)
}

/// Holds the swing button until `done`, then releases it.
fn swing_until(t: &mut TestApp, what: &str, done: impl Fn(&mut TestApp) -> bool) {
    t.mouse_down(SWING_BUTTON);
    let mut frames = 0;
    while !done(t) {
        assert!(frames < MAX_FRAMES, "{what} within {MAX_FRAMES} frames");
        t.step();
        frames += 1;
    }
    t.mouse_up(SWING_BUTTON);
}

#[test]
#[ignore]
fn hit_feedback_close() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "hit_feedback_close", run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let basalt = material(t, "basalt");

    // A basalt floor high above the terrain, cleared above, and a basalt
    // wall seven cells deep across the view: a broken cell opens a hole
    // with more wall behind it.
    edit_boxes(
        t,
        &[
            (
                EditOp::Set(basalt.id),
                CellPos::new(20, FLOOR_ROW, 4),
                CellPos::new(43, FLOOR_ROW, 59),
            ),
            (
                EditOp::Remove,
                CellPos::new(20, FLOOR_ROW + 1, 4),
                CellPos::new(43, 124, 59),
            ),
            (
                EditOp::Set(basalt.id),
                CellPos::new(24, FLOOR_ROW + 1, WALL_NEAR_Z - 6),
                CellPos::new(42, 112, WALL_NEAR_Z),
            ),
        ],
    );
    for (_, _, x) in SHOTS {
        assert_eq!(
            t.host_world()
                .get_cell(CellPos::new(x, EYE_ROW, WALL_NEAR_Z)),
            Some(basalt.id)
        );
    }

    // The outline and flash lines are depth-tested.
    let bias = t
        .app()
        .world()
        .resource::<GizmoConfigStore>()
        .config::<HudGizmos>()
        .0
        .depth_bias;
    assert_eq!(bias, HUD_GIZMO_DEPTH_BIAS);
    assert_eq!(bias, 0.0, "no bias toward the camera");

    let column_m = |x: i32| (x as f32 + 0.5) * CELL_SIZE_M;
    t.spawn_player(Vec3::new(
        column_m(SHOTS[0].2),
        FLOOR_M,
        WALL_FACE_M + SHOTS[0].1,
    ));
    for (label, distance_m, x) in SHOTS {
        let target = CellPos::new(x, EYE_ROW, WALL_NEAR_Z);
        {
            let world = t.app_mut().world_mut();
            let mut player = world
                .query::<&mut Player>()
                .single_mut(world)
                .expect("one player");
            player.feet = Vec3::new(column_m(x), FLOOR_M, WALL_FACE_M + distance_m);
            player.velocity = Vec3::ZERO;
        }
        t.steps(30);
        assert!(t.player().grounded, "{label}: standing: {:?}", t.player());
        let eye = t.player().eye;
        assert!(
            (WALL_FACE_M + distance_m - eye.z).abs() < 1e-3,
            "{label}: the eye is {distance_m} m from the face: {eye}"
        );
        let face = Vec3::new(
            column_m(x),
            (EYE_ROW as f32 + 0.5) * CELL_SIZE_M,
            WALL_FACE_M,
        );
        bots::look_at(t, face).expect("aims");
        settle(t);
        assert_eq!(t.hud().target, Some(target), "{label}: aimed at the target");
        assert!(cracks(t).is_empty(), "{label}: no damage, no overlay");

        // Mid-damage: the overlay on the target, as strong as the damage.
        swing_until(t, "half the damage", |t| {
            t.hud().damage.is_some_and(|d| d.fraction() >= MID_FRACTION)
        });
        t.steps(2);
        let fraction = t.hud().damage.expect("damaged").fraction();
        assert!(fraction < 1.0, "{label}: not broken yet ({fraction})");
        let found = cracks(t);
        assert_eq!(found.len(), 1, "{label}: one overlay");
        let (crack, color) = found[0];
        assert_eq!(crack.cell, target);
        assert_eq!(crack.strength, fraction, "{label}: strength is the damage");
        assert_eq!(color, crack_color(fraction));
        eprintln!("{label}: damage fraction {fraction}");
        shoot(t, &format!("{label}-mid"));

        // The break: solid chips fly, the overlay is gone.
        swing_until(t, "the break", |t| {
            replica_cell(t, target) == Some(MaterialId::AIR)
        });
        t.step();
        assert_eq!(t.hud().last_break.map(|b| b.cell), Some(target));
        assert_eq!(chips(t), (BREAK_CHIPS, BREAK_CHIPS), "{label}: chip meshes");
        assert!(cracks(t).is_empty(), "{label}: the overlay is gone");
        t.steps(2);
        shoot(t, &format!("{label}-break"));

        // The effect ends: the chips are despawned.
        t.steps(40);
        assert_eq!(t.hud().last_break, None);
        assert_eq!(chips(t), (0, 0), "{label}: chips despawned");
    }
    shoot(t, "after");
}
