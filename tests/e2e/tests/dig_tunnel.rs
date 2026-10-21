//! Digging a tunnel (SAD §12.3, §12.4): TunnelBot digs a tunnel 10 cells
//! into a thick wall with the pickaxe, walking in as it goes. The player
//! ends at least 4 m deeper into the wall, every tunnel cell is Air on the
//! host and the replica, the rest of the wall stands, the dug cells are in
//! the inventory, and the host and replica worlds hash the same.

use bevy::prelude::*;
use image::RgbaImage;
use rmc_client::{MeshQueue, MeshStats, Pickaxe, Replica};
use rmc_protocol::ClientMsg;
use rmc_testkit::bots::TunnelBot;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialDef, MaterialId, Phase};
use rmc_world::shape::Shape;

/// Cells the tunnel goes forward.
const LENGTH: u32 = 10;
/// How far into the wall the player's feet must end, in meters.
const MIN_DEPTH_M: f32 = 4.0;
/// Top of the course floor (cell layer y = 100), in meters.
const FLOOR_M: f32 = 101.0 * CELL_SIZE_M;
/// The wall: 16 cells thick along Z, its near (+Z) face at z = 18 m.
const WALL_MIN: CellPos = CellPos::new(26, 101, 20);
const WALL_MAX: CellPos = CellPos::new(42, 112, 35);
/// The player's feet: 1.95 m in front of the wall's face, in the middle of
/// cell column x = 34.
const FEET: Vec3 = Vec3::new(17.25, FLOOR_M, 20.25);

/// Half the side of the sampled pixel block around the image center.
const BLOCK_HALF_PX: i64 = 16;
/// Pixels this close to the center on both axes are the crosshair's and
/// are left out of the sample.
const CROSSHAIR_HALF_PX: i64 = 8;
/// The center block must change by at least this much between the wall
/// before the dig and the view back out of the tunnel (mean over the three
/// channels of the absolute difference of the block's mean colors).
const MIN_CHANGE: f32 = 24.0;
/// The view back out of the tunnel is no flat image: its 5th and 95th
/// percentile luminances differ by at least this much.
const MIN_SPREAD: f32 = 20.0;

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

/// The 5th to 95th percentile spread of the image's luminance (Rec. 709
/// weights on the sRGB bytes).
fn luminance_spread(image: &RgbaImage) -> f32 {
    let mut lum: Vec<f32> = image
        .pixels()
        .map(|p| 0.2126 * f32::from(p[0]) + 0.7152 * f32::from(p[1]) + 0.0722 * f32::from(p[2]))
        .collect();
    lum.sort_by(f32::total_cmp);
    let at = |q: usize| lum[(lum.len() - 1) * q / 100];
    at(95) - at(5)
}

fn wall_cells() -> impl Iterator<Item = CellPos> {
    (WALL_MIN.0.x..=WALL_MAX.0.x).flat_map(|x| {
        (WALL_MIN.0.y..=WALL_MAX.0.y)
            .flat_map(move |y| (WALL_MIN.0.z..=WALL_MAX.0.z).map(move |z| CellPos::new(x, y, z)))
    })
}

#[test]
#[ignore]
fn dig_tunnel() {
    // 2 × 4 × 2 chunks, full height. The tunnel takes about 1,800 frames;
    // a quarter of the default pixels halves a frame's cost on WARP.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 127, 63)),
        size: (320, 180),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "dig_tunnel", dig);
}

fn dig(t: &mut TestApp) {
    let clock = std::time::Instant::now();
    settle(t);

    // The wall is the plain diggable solid that takes the fewest swings, so
    // the 80-cell tunnel stays quick on a software rasterizer.
    let profile = t.app().world().resource::<Pickaxe>().0;
    let wall: MaterialDef = t
        .host_world()
        .materials()
        .iter()
        .filter(|d| d.phase == Phase::Solid && d.emissive == 0.0 && d.diggable)
        .min_by_key(|d| d.hardness.div_ceil(profile.damage))
        .expect("assets/materials.ron has a plain diggable solid")
        .clone();
    eprintln!("wall {} (hardness {})", wall.key, wall.hardness);

    // The course, high above the terrain: a floor with its top at FLOOR_M,
    // everything above it cleared, and the thick wall across the view.
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
            (EditOp::Set(wall.id), WALL_MIN, WALL_MAX),
        ],
    );
    assert!(
        wall_cells().all(|c| t.host_world().get_cell(c) == Some(wall.id)),
        "the wall was built"
    );
    assert_eq!(replica_hash(t), t.host_world().hash());
    let wall_face_m = (WALL_MAX.0.z + 1) as f32 * CELL_SIZE_M;

    t.spawn_player(FEET);
    t.steps(30);
    assert!(t.player().grounded, "landed on the floor: {:?}", t.player());
    settle(t);
    let before = t.screenshot();
    let path = e2e::save_screenshot(&before, "dig_tunnel", t.frame());
    eprintln!("[{:.0?}] before: {}", clock.elapsed(), path.display());

    let held = t.inventory().count(wall.id);
    let bot = TunnelBot::new(LENGTH);
    let report = bot.run(t).unwrap_or_else(|e| panic!("TunnelBot: {e}"));
    eprintln!(
        "dug {} cells with {} swings in {} frames; feet {:?}",
        report.cells.len(),
        report.swings,
        report.frames,
        t.player().pos
    );

    // The tunnel: LENGTH slices back from the wall's face, the bot's width
    // centered on the player's column, the bot's height up from the floor.
    let (width, height) = (TunnelBot::width_cells(), TunnelBot::height_cells());
    let first_column = report.first_column;
    assert_eq!(report.width, width, "the box fits one column: no wider");
    let tunnel: Vec<CellPos> = (0..LENGTH as i32)
        .flat_map(|k| {
            (0..width).flat_map(move |i| {
                (0..height).map(move |h| {
                    CellPos::new(first_column + i, WALL_MIN.0.y + h, WALL_MAX.0.z - k)
                })
            })
        })
        .collect();
    assert_eq!(height, 4, "4 high");
    assert_eq!(tunnel.len(), LENGTH as usize * (width * height) as usize);
    let mut dug = report.cells.clone();
    dug.sort_by_key(|c| c.0.to_array());
    let mut want = tunnel.clone();
    want.sort_by_key(|c| c.0.to_array());
    assert_eq!(dug, want, "the bot dug exactly the tunnel");
    for &c in &tunnel {
        assert_eq!(t.host_world().get_cell(c), Some(MaterialId::AIR), "{c:?}");
        assert_eq!(replica_cell(t, c), Some(MaterialId::AIR), "{c:?}");
    }
    let standing = wall_cells()
        .filter(|c| !tunnel.contains(c))
        .filter(|&c| t.host_world().get_cell(c) == Some(wall.id))
        .count();
    let wall_count = wall_cells().count();
    assert_eq!(
        standing,
        wall_count - tunnel.len(),
        "the rest of the wall stands"
    );
    let swings = u64::from(wall.hardness.div_ceil(profile.damage)) * tunnel.len() as u64;
    assert_eq!(report.swings, swings, "hardness / damage swings per cell");

    // The player walked in and stands in the tunnel.
    let p = t.player();
    let depth = wall_face_m - p.pos.z;
    assert!(
        report.start.z - p.pos.z >= MIN_DEPTH_M,
        "walked in from {:?}: {p:?}",
        report.start
    );
    eprintln!("[{:.0?}] feet {depth:.2} m into the wall", clock.elapsed());
    assert!(depth >= MIN_DEPTH_M, "feet {depth} m into the wall: {p:?}");
    assert!(p.grounded, "standing: {p:?}");
    assert!(
        (p.pos.y - FLOOR_M).abs() < 0.01,
        "on the tunnel floor: {p:?}"
    );
    assert!(t.player_penetration().is_empty());

    // Every dug cell is in the inventory, on the client and on the host.
    let gained = u32::from(wall.yield_count) * tunnel.len() as u32;
    assert!(gained > 0, "{} yields something", wall.key);
    assert_eq!(t.inventory().count(wall.id), held + gained);
    assert_eq!(
        t.host().inventory(t.player_id()).map(|i| i.count(wall.id)),
        Some(held + gained)
    );

    // Host and replica agree on every cell.
    t.step();
    assert_eq!(
        replica_hash(t),
        t.host_world().hash(),
        "host and replica hashes"
    );

    // Look back out of the tunnel, past the open course to the world's
    // shell, for the screenshot.
    let sensitivity = t
        .app()
        .world()
        .resource::<rmc_client::ControllerConfig>()
        .sensitivity_rad;
    t.look(std::f32::consts::PI / sensitivity, 0.0);
    t.step();
    settle(t);
    eprintln!(
        "[{:.0?}] turned around at frame {}",
        clock.elapsed(),
        t.frame()
    );
    let after = t.screenshot();
    let path = e2e::save_screenshot(&after, "dig_tunnel", t.frame());
    eprintln!("[{:.0?}] after: {}", clock.elapsed(), path.display());
    let (a, b) = (center_block(&before), center_block(&after));
    let change = (0..3).map(|c| (a[c] - b[c]).abs()).sum::<f32>() / 3.0;
    let spread = luminance_spread(&after);
    eprintln!("center block {a:?} -> {b:?}: change {change:.1}; spread {spread:.1}");
    assert!(
        change >= MIN_CHANGE,
        "the wall gave way to the view out of the tunnel: {a:?} -> {b:?}"
    );
    assert!(
        spread >= MIN_SPREAD,
        "the tunnel view is not flat: {spread}"
    );
    assert_eq!(replica_hash(t), t.host_world().hash());
    assert_eq!(t.render_failure(), None);
}
