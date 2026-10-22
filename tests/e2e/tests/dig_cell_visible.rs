//! Digging a cell with the pickaxe (SAD §11.5, §12.4): holding the swing
//! button at a wall cell swings until it breaks; the replica cell becomes
//! Air, the inventory count goes up by the material's yield, and the pixel
//! block under the crosshair changes color.

use bevy::prelude::*;
use image::RgbaImage;
use rmc_client::{
    ControllerConfig, MeshQueue, MeshStats, Pickaxe, Replica, SWING_BUTTON, SwingState,
};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialDef, MaterialId, Phase};
use rmc_world::shape::Shape;

/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;
/// The wall cell the player digs: interior to its chunk on every axis.
const TARGET: CellPos = CellPos::new(34, 104, 36);
/// The player's feet: centered on the target's column, 1.75 m from the
/// wall's near face (inside the pickaxe's reach).
const FEET: Vec3 = Vec3::new(17.25, FLOOR_M, 20.25);
/// Half the side of the sampled pixel block around the image center.
const BLOCK_HALF_PX: i64 = 24;
/// Pixels this close to the center on both axes are the crosshair's and
/// are left out of the sample.
const CROSSHAIR_HALF_PX: i64 = 8;
/// The sampled block must change by at least this much (mean over the
/// three channels of the absolute difference of the block's mean colors).
const MIN_CHANGE: f32 = 24.0;

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

fn plain_solids(t: &TestApp) -> Vec<MaterialDef> {
    t.host_world()
        .materials()
        .iter()
        .filter(|d| d.phase == Phase::Solid && !d.passable && d.emissive == 0.0 && d.diggable)
        .cloned()
        .collect()
}

/// The log of an sRGB color's luminance (Rec. 709 weights, plus one so
/// black has one). Tonemapping compresses bright colors, so contrast is a
/// luminance ratio rather than a difference.
fn log_luminance(c: [u8; 4]) -> f32 {
    (1.0 + 0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])).ln()
}

/// The mean RGB of the block around the image center, crosshair left out.
fn center_block(image: &RgbaImage) -> [f32; 3] {
    let (cx, cy) = (i64::from(image.width() / 2), i64::from(image.height() / 2));
    let mut sum = [0u64; 3];
    let mut n = 0u64;
    for dy in -BLOCK_HALF_PX..BLOCK_HALF_PX {
        for dx in -BLOCK_HALF_PX..BLOCK_HALF_PX {
            if dx.abs() <= CROSSHAIR_HALF_PX && dy.abs() <= CROSSHAIR_HALF_PX {
                continue;
            }
            let p = image.get_pixel((cx + dx) as u32, (cy + dy) as u32);
            for (c, s) in sum.iter_mut().enumerate() {
                *s += u64::from(p.0[c]);
            }
            n += 1;
        }
    }
    sum.map(|s| s as f32 / n as f32)
}

fn replica_cell(t: &TestApp, cell: CellPos) -> Option<MaterialId> {
    t.app().world().resource::<Replica>().world().get_cell(cell)
}

#[test]
#[ignore]
fn dig_cell_visible() {
    // 2 × 4 × 2 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "dig_cell_visible", dig);
}

fn dig(t: &mut TestApp) {
    settle(t);

    // The wall takes more than one swing; the backdrop behind it is the
    // plain solid with the largest luminance ratio to the wall, so the
    // hole shows a different color through lighting and tonemapping.
    let profile = t.app().world().resource::<Pickaxe>().0;
    let solids = plain_solids(t);
    let wall = solids
        .iter()
        .find(|d| d.hardness > profile.damage)
        .expect("assets/materials.ron has a plain solid that takes several swings")
        .clone();
    let backdrop = solids
        .iter()
        .max_by(|a, b| {
            let contrast =
                |d: &MaterialDef| (log_luminance(d.color) - log_luminance(wall.color)).abs();
            contrast(a).total_cmp(&contrast(b))
        })
        .expect("plain solids exist")
        .clone();
    eprintln!("wall {}, backdrop {}", wall.key, backdrop.key);

    // The course, high above the terrain: a floor with its top at FLOOR_M,
    // everything above it cleared, a one-cell-thick wall across the view
    // with TARGET in it, and the backdrop 2.5 m behind the wall, out of
    // the pickaxe's reach.
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
            (
                EditOp::Set(backdrop.id),
                CellPos::new(20, 101, 30),
                CellPos::new(43, 124, 30),
            ),
        ],
    );
    assert_eq!(t.host_world().get_cell(TARGET), Some(wall.id));
    // The backdrop cell straight behind the target.
    let behind = CellPos::new(TARGET.0.x, TARGET.0.y, 30);
    assert_eq!(t.host_world().get_cell(behind), Some(backdrop.id));

    t.spawn_player(FEET);
    t.steps(30);
    assert!(t.player().grounded, "landed on the floor: {:?}", t.player());

    // Aim at the middle of the target's near face.
    let config = t.app().world().resource::<ControllerConfig>().clone();
    let eye = t.player().eye;
    let face = Vec3::new(
        (TARGET.0.x as f32 + 0.5) * CELL_SIZE_M,
        (TARGET.0.y as f32 + 0.5) * CELL_SIZE_M,
        (TARGET.0.z + 1) as f32 * CELL_SIZE_M,
    );
    let pitch = (face.y - eye.y).atan2(eye.z - face.z);
    t.look(0.0, -pitch / config.sensitivity_rad);
    t.step();
    settle(t);

    let hud = t.hud();
    assert_eq!(hud.target, Some(TARGET), "the crosshair is on the target");
    assert_eq!(hud.material.as_deref(), Some(wall.key.as_str()));
    let held = t.inventory().count(wall.id);
    assert_eq!(t.app().world().resource::<SwingState>().swings(), 0);
    let before = t.screenshot();
    let path = e2e::save_screenshot(&before, "dig_cell_visible", t.frame());
    eprintln!("before: {}", path.display());

    // Hold the swing button until the cell breaks.
    let swings = wall.hardness.div_ceil(profile.damage);
    let interval = (profile.interval_s * 60.0).ceil() as u32;
    let limit = u32::from(swings) * interval + 10;
    t.mouse_down(SWING_BUTTON);
    let mut frames = 0;
    while replica_cell(t, TARGET) != Some(MaterialId::AIR) {
        assert!(
            frames < limit,
            "the target broke within {limit} frames; HUD {:?}",
            t.hud()
        );
        t.step();
        frames += 1;
    }
    t.mouse_up(SWING_BUTTON);
    t.step();
    let swung = t.app().world().resource::<SwingState>().swings();
    eprintln!("broke after {frames} frames and {swung} swings");
    assert_eq!(swung, u64::from(swings), "hardness / damage swings");
    assert_eq!(t.host_world().get_cell(TARGET), Some(MaterialId::AIR));
    assert_eq!(replica_cell(t, TARGET), Some(MaterialId::AIR));

    // The host's Inventory reached the client and the HUD.
    let yielded = u32::from(wall.yield_count);
    assert!(yielded > 0, "{} yields something", wall.key);
    assert_eq!(t.inventory().count(wall.id), held + yielded);
    assert_eq!(
        t.host().inventory(t.player_id()).map(|i| i.count(wall.id)),
        Some(held + yielded)
    );
    let hud = t.hud();
    assert!(
        hud.inventory
            .iter()
            .any(|item| item.material == wall.id && item.count == held + yielded),
        "the HUD lists the dug material: {:?}",
        hud.inventory
    );
    assert_ne!(hud.target, Some(TARGET), "the broken cell is no target");

    // The pixels under the crosshair now show the backdrop through the hole.
    settle(t);
    let chunk = TARGET.chunk();
    let revision = t
        .app()
        .world()
        .resource::<Replica>()
        .world()
        .chunk(chunk)
        .map(|c| c.revision);
    assert!(revision.is_some());
    assert_eq!(
        t.app()
            .world()
            .resource::<MeshQueue>()
            .meshed_revision(chunk),
        revision,
        "the hole is meshed"
    );
    let after = t.screenshot();
    let path = e2e::save_screenshot(&after, "dig_cell_visible", t.frame());
    eprintln!("after: {}", path.display());
    let (a, b) = (center_block(&before), center_block(&after));
    let change = (0..3).map(|c| (a[c] - b[c]).abs()).sum::<f32>() / 3.0;
    eprintln!("center block {a:?} -> {b:?}: change {change:.1}");
    assert!(
        change >= MIN_CHANGE,
        "the block under the crosshair changed color: {a:?} -> {b:?}"
    );
    assert_eq!(t.render_failure(), None);
}
