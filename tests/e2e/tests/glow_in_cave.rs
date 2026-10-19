//! Glow where it matters (SAD §11.2, F092): a glowcrystal patch set into a
//! stone wall inside a closed, dark chamber (no sun, lantern off, ambient
//! light only), as in a cave.
//!
//! The camera faces the wall. The crystal's pixels are found by projecting
//! the patch's corners through the game camera; the surrounding stone is a
//! ring around them. With emissive on, the crystal is brighter than the
//! stone by at least [`MIN_GLOW_MARGIN`]; with the glow scaled to zero the
//! margin shrinks by at least [`MIN_GLOW_GAIN`], so the difference is the
//! glow and not only the crystal's lighter color.

use bevy::prelude::*;
use image::RgbaImage;
use rmc_client::render::Lantern;
use rmc_client::{ChunkEmissiveMaterial, EmissiveChunkMaterial, GameCamera, MeshQueue, MeshStats};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialDef, Phase};
use rmc_world::shape::Shape;

/// With emissive on, the crystal's mean luminance exceeds the surrounding
/// stone's by at least this much (Rec. 709 luminance of the sRGB bytes).
/// F092 measured on WARP: crystal 190.0 and stone 83.7 with emissive on
/// (margin 106.3), crystal 120.7 and stone 82.7 with it off (margin 38.0).
/// This is a little over half the measured margin.
const MIN_GLOW_MARGIN: f64 = 60.0;
/// The crystal-over-stone margin with emissive on exceeds the margin with
/// the glow scaled to zero by at least this much: under half the measured
/// gain (68.3), while the crystal's color alone (off) stays well under it.
const MIN_GLOW_GAIN: f64 = 30.0;
/// The stone around the crystal stays dark: the room is a cave, not a lit
/// scene in which any light color would stand out. Stone's own color has
/// luminance 128; the ambient-only stone measured 83.7.
const MAX_STONE_LUMINANCE: f64 = 110.0;

/// The chamber's shell (outer box) and the wall standing inside it, whose
/// face toward -Z is at z = [`WALL_FACE_Z`] cells.
const SHELL_MIN: CellPos = CellPos::new(36, 98, 20);
const SHELL_MAX: CellPos = CellPos::new(91, 125, 75);
const WALL_FACE_Z: i32 = 56;
const WALL_MIN: CellPos = CellPos::new(40, 99, WALL_FACE_Z);
const WALL_MAX: CellPos = CellPos::new(87, 124, 60);
/// The glowcrystal patch, flush with the wall's face: 4 × 4 cells (2 m).
const CRYSTAL_MIN: CellPos = CellPos::new(62, 110, WALL_FACE_Z);
const CRYSTAL_MAX: CellPos = CellPos::new(65, 113, WALL_FACE_Z + 1);
/// How far the camera stands from the wall's face, in meters.
const VIEW_M: f32 = 6.0;

/// The crystal sample is the patch's projected rectangle shrunk by this
/// fraction of its size on each side, away from the edge pixels.
const INSET: f32 = 0.15;
/// The stone sample is the ring between the patch's rectangle grown by
/// [`RING_GAP`] and by [`RING_OUTER`] of its size on each side. The gap
/// keeps edge pixels and the glow's bloom halo out of the stone sample.
const RING_GAP: f32 = 0.25;
const RING_OUTER: f32 = 0.5;
/// Pixels this close to the image center on both axes are the crosshair's
/// and are left out of both samples.
const CROSSHAIR_HALF_PX: f32 = 8.0;

fn luminance(p: &image::Rgba<u8>) -> f64 {
    (0.2126 * f64::from(p.0[0])) + (0.7152 * f64::from(p.0[1])) + (0.0722 * f64::from(p.0[2]))
}

/// A pixel rectangle, `min` inclusive and `max` exclusive.
#[derive(Clone, Copy, Debug)]
struct PxRect {
    min: Vec2,
    max: Vec2,
}

impl PxRect {
    /// Grown (or, for a negative `f`, shrunk) by `f` of its size per side.
    fn grown(self, f: f32) -> Self {
        let pad = (self.max - self.min) * f;
        Self {
            min: self.min - pad,
            max: self.max + pad,
        }
    }

    fn contains(self, p: Vec2) -> bool {
        p.cmpge(self.min).all() && p.cmplt(self.max).all()
    }
}

/// Mean luminance of the pixels inside `outer`, outside `hole` (if any)
/// and away from the crosshair. Panics if `outer` leaves the image.
fn mean_luminance(image: &RgbaImage, outer: PxRect, hole: Option<PxRect>) -> f64 {
    let (w, h) = (image.width() as f32, image.height() as f32);
    assert!(
        outer.min.cmpge(Vec2::ZERO).all() && outer.max.cmple(Vec2::new(w, h)).all(),
        "the sample {outer:?} lies inside the {w} × {h} image"
    );
    let center = Vec2::new(w, h) / 2.0;
    let mut sum = 0.0;
    let mut n = 0u32;
    for y in outer.min.y.ceil() as u32..outer.max.y.ceil() as u32 {
        for x in outer.min.x.ceil() as u32..outer.max.x.ceil() as u32 {
            let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            if hole.is_some_and(|r| r.contains(p))
                || ((p - center).abs().cmple(Vec2::splat(CROSSHAIR_HALF_PX))).all()
            {
                continue;
            }
            sum += luminance(image.get_pixel(x, y));
            n += 1;
        }
    }
    assert!(n > 100, "the sample {outer:?} has {n} pixels");
    sum / f64::from(n)
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

fn material(t: &TestApp, key: &str) -> MaterialDef {
    t.host_world()
        .materials()
        .iter()
        .find(|d| d.key == key)
        .unwrap_or_else(|| panic!("assets/materials.ron has {key}"))
        .clone()
}

/// The crystal patch's front face, projected through the game camera into
/// screenshot pixels of a `size` image.
fn crystal_rect(t: &mut TestApp, size: UVec2) -> PxRect {
    let world = t.app_mut().world_mut();
    let (camera, transform) = world
        .query_filtered::<(&Camera, &GlobalTransform), With<GameCamera>>()
        .single(world)
        .expect("one game camera");
    let viewport = camera
        .logical_viewport_size()
        .expect("the game camera has a viewport");
    let scale = size.as_vec2() / viewport;
    let z = WALL_FACE_Z as f32 * CELL_SIZE_M;
    let (x0, x1) = (
        CRYSTAL_MIN.0.x as f32 * CELL_SIZE_M,
        (CRYSTAL_MAX.0.x + 1) as f32 * CELL_SIZE_M,
    );
    let (y0, y1) = (
        CRYSTAL_MIN.0.y as f32 * CELL_SIZE_M,
        (CRYSTAL_MAX.0.y + 1) as f32 * CELL_SIZE_M,
    );
    let mut min = Vec2::splat(f32::MAX);
    let mut max = Vec2::splat(f32::MIN);
    for corner in [
        Vec3::new(x0, y0, z),
        Vec3::new(x0, y1, z),
        Vec3::new(x1, y0, z),
        Vec3::new(x1, y1, z),
    ] {
        let p = camera
            .world_to_viewport(transform, corner)
            .unwrap_or_else(|e| panic!("{corner:?} projects into the view: {e:?}"))
            * scale;
        min = min.min(p);
        max = max.max(p);
    }
    PxRect { min, max }
}

/// Crystal and stone mean luminance in `image`.
fn sample(image: &RgbaImage, crystal: PxRect) -> (f64, f64) {
    let glow = mean_luminance(image, crystal.grown(-INSET), None);
    let stone = mean_luminance(
        image,
        crystal.grown(RING_OUTER),
        Some(crystal.grown(RING_GAP)),
    );
    (glow, stone)
}

#[test]
#[ignore]
fn glow_in_cave() {
    // 4 × 4 × 4 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(127, 127, 127)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "glow_in_cave", run);
}

fn run(t: &mut TestApp) {
    settle(t);
    let stone = material(t, "stone");
    // The first emissive solid in the table: the crystal.
    let crystal = t
        .host_world()
        .materials()
        .iter()
        .find(|d| d.phase == Phase::Solid && d.emissive > 0.0)
        .expect("assets/materials.ron has an emissive solid material")
        .clone();
    eprintln!(
        "glowing material: {:?} (strength {}), wall: {:?}",
        crystal.key, crystal.emissive, stone.key
    );

    // A closed stone chamber near the top of the world, hollowed out, with
    // a wall standing on its floor and the crystal set into the wall's face.
    let edits = [
        (EditOp::Set(stone.id), SHELL_MIN, SHELL_MAX),
        (
            EditOp::Remove,
            CellPos::new(SHELL_MIN.0.x + 1, SHELL_MIN.0.y + 1, SHELL_MIN.0.z + 1),
            CellPos::new(SHELL_MAX.0.x - 1, SHELL_MAX.0.y - 1, SHELL_MAX.0.z - 1),
        ),
        (EditOp::Set(stone.id), WALL_MIN, WALL_MAX),
        (EditOp::Set(crystal.id), CRYSTAL_MIN, CRYSTAL_MAX),
    ];
    for (seq, (op, min, max)) in (1..).zip(edits) {
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
    settle(t);
    let host = t.host_world();
    let mid = CellPos::new(64, 112, WALL_FACE_Z);
    assert_eq!(host.get_cell(mid), Some(crystal.id));
    assert_eq!(
        host.get_cell(CellPos::new(70, 112, WALL_FACE_Z)),
        Some(stone.id)
    );
    assert!(
        !host.is_solid(CellPos::new(64, 112, WALL_FACE_Z - 1)),
        "the chamber is hollow in front of the wall"
    );

    // Facing the crystal's center from VIEW_M, lantern off: only the
    // ambient light reaches the wall.
    {
        let world = t.app_mut().world_mut();
        let x = (CRYSTAL_MIN.0.x + CRYSTAL_MAX.0.x + 1) as f32 * CELL_SIZE_M / 2.0;
        let y = (CRYSTAL_MIN.0.y + CRYSTAL_MAX.0.y + 1) as f32 * CELL_SIZE_M / 2.0;
        let z = WALL_FACE_Z as f32 * CELL_SIZE_M - VIEW_M;
        let mut camera = world
            .query_filtered::<&mut Transform, With<GameCamera>>()
            .single_mut(world)
            .expect("one game camera");
        *camera = Transform::from_xyz(x, y, z).looking_to(Vec3::Z, Vec3::Y);
        for mut light in world
            .query_filtered::<&mut PointLight, With<Lantern>>()
            .iter_mut(world)
        {
            light.intensity = 0.0;
        }
    }
    t.steps(2);

    let lit = t.screenshot();
    let path = e2e::save_screenshot(&lit, "glow_in_cave", t.frame());
    eprintln!("screenshot: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "render failure: {:?}",
        t.render_failure()
    );
    let rect = crystal_rect(t, UVec2::new(lit.width(), lit.height()));
    eprintln!(
        "crystal pixels: {rect:?} in {} × {}",
        lit.width(),
        lit.height()
    );
    let (glow_on, stone_on) = sample(&lit, rect);

    // The same view with the glow scaled to zero.
    let handle = t
        .app()
        .world()
        .get_resource::<EmissiveChunkMaterial>()
        .expect("a rendering app has the emissive chunk material")
        .0
        .clone();
    t.app_mut()
        .world_mut()
        .resource_mut::<Assets<ChunkEmissiveMaterial>>()
        .get_mut(&handle)
        .expect("emissive material asset")
        .extension
        .emissive_scale = 0.0;
    t.steps(2);
    let unlit = t.screenshot();
    let path = e2e::save_screenshot(&unlit, "glow_in_cave-off", t.frame());
    eprintln!("screenshot without glow: {}", path.display());
    assert!(
        t.render_failure().is_none(),
        "render failure: {:?}",
        t.render_failure()
    );
    let (glow_off, stone_off) = sample(&unlit, rect);

    let (margin_on, margin_off) = (glow_on - stone_on, glow_off - stone_off);
    eprintln!(
        "emissive on: crystal {glow_on:.1}, stone {stone_on:.1}, margin {margin_on:.1}; \
         off: crystal {glow_off:.1}, stone {stone_off:.1}, margin {margin_off:.1}"
    );
    assert!(
        stone_on < MAX_STONE_LUMINANCE,
        "the cave is dark: stone {stone_on:.1} < {MAX_STONE_LUMINANCE}"
    );
    assert!(
        stone_off > 1.0,
        "the stone is drawn and lit by the ambient light ({stone_off:.1})"
    );
    assert!(
        margin_on >= MIN_GLOW_MARGIN,
        "the crystal stands out from the stone: {glow_on:.1} vs {stone_on:.1}, \
         margin {margin_on:.1} < {MIN_GLOW_MARGIN}"
    );
    assert!(
        margin_on >= margin_off + MIN_GLOW_GAIN,
        "the glow widens the margin: {margin_on:.1} with it, {margin_off:.1} without"
    );
}
