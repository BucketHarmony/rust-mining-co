//! Surface variation (owner change F097, extending SAD §11.2): every cell's
//! surface brightness varies by a few percent, from a hash of the cell
//! taken in the fragment shader, so large greedy-merged faces read as rock
//! and soil rather than flat paint.
//!
//! Three views: a basalt wall face-on at 1.5 m, a topsoil field looking
//! down, and a stone tunnel from its mouth. For each, the test asserts what
//! the code set up (CLAUDE.md rule 21): every chunk mesh is drawn with the
//! one emissive chunk material, whose jitter uniforms are the named
//! constants; the cells in view hash to many different factors within the
//! amplitude; and the frame is not blank. The wall is also shot with the
//! amplitude set to 0, twice: those two frames must be the same and must
//! differ from the jittered one, so the shader's jitter reaches the
//! picture. Every shot is saved for the playtester; there are no luminance
//! or color thresholds.

use std::collections::BTreeSet;

use bevy::prelude::*;
use image::RgbaImage;
use rmc_client::{
    CELL_JITTER_AMPLITUDE, CELL_JITTER_INSET_M, ChunkEmissiveMaterial, ChunkMesh,
    EmissiveChunkMaterial, EmissiveFromAlpha, GameCamera, MeshQueue, MeshStats, cell_jitter,
    fragment_cell,
};
use rmc_protocol::ClientMsg;
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp};
use rmc_world::materials::{MaterialDef, MaterialId};
use rmc_world::shape::Shape;

/// The basalt wall; its face toward -Z is at z = [`WALL_FACE_Z`] cells.
const WALL_FACE_Z: i32 = 40;
const WALL_MIN: CellPos = CellPos::new(20, 100, WALL_FACE_Z);
const WALL_MAX: CellPos = CellPos::new(50, 124, WALL_FACE_Z + 1);
/// The topsoil field, one cell thick, seen from [`FIELD_EYE_M`] above.
const FIELD_ROW: i32 = 100;
const FIELD_MIN: CellPos = CellPos::new(20, FIELD_ROW, 70);
const FIELD_MAX: CellPos = CellPos::new(60, FIELD_ROW, 110);
const FIELD_EYE_M: f32 = 4.0;
/// The stone block and the tunnel through it along +Z, 3 wide and 4 high.
const BLOCK_MIN: CellPos = CellPos::new(80, 100, 40);
const BLOCK_MAX: CellPos = CellPos::new(100, 115, 100);
const TUNNEL_MIN: CellPos = CellPos::new(89, 101, 40);
const TUNNEL_MAX: CellPos = CellPos::new(91, 104, 100);
/// Eye height above the tunnel floor, and how far in from the mouth.
const TUNNEL_EYE_M: f32 = 1.55;
const TUNNEL_IN_M: f32 = 1.0;
/// Eye row in front of the wall, and the reading distance from it.
const WALL_EYE_ROW: i32 = 112;
const READING_M: f32 = 1.5;
/// Half the sides of the patch of face cells around the view's center
/// whose factors are checked (the tunnel's wall is only 4 cells high), and
/// how many distinct factors that patch must have.
const PATCH_HALF: (i32, i32) = (4, 4);
const TUNNEL_PATCH_HALF: (i32, i32) = (12, 1);
const MIN_DISTINCT_FACTORS: usize = 40;
/// A frame with fewer distinct colors than this is blank (a flat fill).
const MIN_COLORS: usize = 16;

#[test]
#[ignore]
fn surface_variation() {
    // 4 × 4 × 4 chunks, full height.
    let mut t = TestApp::new(TestConfig {
        world_bounds: CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(127, 127, 127)),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "surface_variation", run);
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

fn edit_boxes(t: &mut TestApp, edits: &[(EditOp, CellPos, CellPos)]) {
    for (seq, &(op, min, max)) in (1..).zip(edits) {
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

fn distinct_colors(image: &RgbaImage) -> usize {
    image
        .pixels()
        .map(|p| [p.0[0], p.0[1], p.0[2]])
        .collect::<BTreeSet<_>>()
        .len()
}

/// Places the game camera at `eye` (meters) looking along `dir`.
fn place_camera(t: &mut TestApp, eye: Vec3, dir: Vec3, up: Vec3) {
    let world = t.app_mut().world_mut();
    let mut camera = world
        .query_filtered::<&mut Transform, With<GameCamera>>()
        .single_mut(world)
        .expect("one game camera");
    *camera = Transform::from_translation(eye).looking_to(dir, up);
}

/// Screenshots the view as `surface_variation-<label>` and checks it is not
/// blank.
fn shoot(t: &mut TestApp, label: &str) -> RgbaImage {
    t.steps(3);
    let image = t.screenshot();
    let path = e2e::save_screenshot(&image, &format!("surface_variation-{label}"), t.frame());
    assert_eq!(t.render_failure(), None, "{label}: render failure");
    let colors = distinct_colors(&image);
    eprintln!("{label}: {} ({colors} colors)", path.display());
    assert!(
        colors >= MIN_COLORS,
        "{label}: a blank frame ({colors} colors)"
    );
    image
}

/// Every chunk mesh is drawn with the one emissive chunk material, and that
/// material's jitter uniforms are the named constants, with `amplitude`.
fn assert_jitter_material(t: &mut TestApp, amplitude: f32) {
    let handle = t
        .app()
        .world()
        .get_resource::<EmissiveChunkMaterial>()
        .expect("the emissive chunk material exists in a rendering app")
        .0
        .clone();
    let world = t.app_mut().world_mut();
    let mut meshes = 0;
    for (chunk, material) in world
        .query_filtered::<(&ChunkMesh, Option<&MeshMaterial3d<ChunkEmissiveMaterial>>), With<Mesh3d>>()
        .iter(world)
    {
        let material = material
            .unwrap_or_else(|| panic!("chunk {:?} has the emissive material", chunk.pos));
        assert_eq!(material.0, handle, "chunk {:?}: the shared material", chunk.pos);
        meshes += 1;
    }
    assert!(meshes > 0, "chunk meshes are drawn");
    let materials = world.resource::<Assets<ChunkEmissiveMaterial>>();
    let ext = &materials.get(&handle).expect("material asset").extension;
    assert_eq!(
        *ext,
        EmissiveFromAlpha {
            jitter_amplitude: amplitude,
            ..EmissiveFromAlpha::default()
        }
    );
    assert_eq!(ext.jitter_cell_size_m, CELL_SIZE_M);
    assert_eq!(ext.jitter_inset_m, CELL_JITTER_INSET_M);
}

fn set_amplitude(t: &mut TestApp, amplitude: f32) {
    let handle = t
        .app()
        .world()
        .resource::<EmissiveChunkMaterial>()
        .0
        .clone();
    let world = t.app_mut().world_mut();
    let mut materials = world.resource_mut::<Assets<ChunkEmissiveMaterial>>();
    materials
        .get_mut(&handle)
        .expect("material asset")
        .extension
        .jitter_amplitude = amplitude;
}

/// The face cells in view hash to many factors, all within the amplitude.
/// `center` is the point at the view's center on a face with outward
/// `normal`; `(u, v)` span the face and `half` gives the patch's extent
/// along each.
fn assert_cells_vary(
    label: &str,
    (center, normal): (Vec3, Vec3),
    (u, v): (IVec3, IVec3),
    half: (i32, i32),
    expect: CellPos,
) {
    let cell = fragment_cell(center, normal);
    assert_eq!(cell, expect.0, "{label}: the center fragment's cell");
    let mut factors = BTreeSet::new();
    for a in -half.0..=half.0 {
        for b in -half.1..=half.1 {
            let j = cell_jitter(cell + u * a + v * b);
            assert!(
                (j - 1.0).abs() <= CELL_JITTER_AMPLITUDE + 1e-6,
                "{label}: factor {j} within 1 ± {CELL_JITTER_AMPLITUDE}"
            );
            factors.insert(j.to_bits());
        }
    }
    assert!(
        factors.len() >= MIN_DISTINCT_FACTORS,
        "{label}: {} distinct factors in the patch",
        factors.len()
    );
}

fn run(t: &mut TestApp) {
    settle(t);
    let basalt = material(t, "basalt");
    let topsoil = material(t, "topsoil");
    let stone = material(t, "stone");

    // Clear the air in front of the wall and above the field, then build
    // the wall, the field, and the stone block with its tunnel.
    let clear_wall = (
        EditOp::Remove,
        CellPos::new(WALL_MIN.0.x, WALL_MIN.0.y, WALL_FACE_Z - 8),
        CellPos::new(WALL_MAX.0.x, WALL_MAX.0.y, WALL_FACE_Z - 1),
    );
    let clear_field = (
        EditOp::Remove,
        CellPos::new(FIELD_MIN.0.x, FIELD_ROW + 1, FIELD_MIN.0.z),
        CellPos::new(FIELD_MAX.0.x, FIELD_ROW + 12, FIELD_MAX.0.z),
    );
    edit_boxes(
        t,
        &[
            clear_wall,
            clear_field,
            (EditOp::Set(basalt.id), WALL_MIN, WALL_MAX),
            (EditOp::Set(topsoil.id), FIELD_MIN, FIELD_MAX),
            (EditOp::Set(stone.id), BLOCK_MIN, BLOCK_MAX),
            (EditOp::Remove, TUNNEL_MIN, TUNNEL_MAX),
        ],
    );
    settle(t);
    let host = t.host_world();
    let wall_eye = CellPos::new(35, WALL_EYE_ROW, WALL_FACE_Z);
    let field_mid = CellPos::new(40, FIELD_ROW, 90);
    let tunnel_wall = CellPos::new(TUNNEL_MIN.0.x - 1, TUNNEL_MIN.0.y + 1, 50);
    let cell = |c: CellPos| host.get_cell(c);
    assert_eq!(cell(wall_eye), Some(basalt.id));
    assert_eq!(cell(field_mid), Some(topsoil.id));
    assert_eq!(cell(tunnel_wall), Some(stone.id));
    assert_eq!(
        cell(CellPos::new(35, WALL_EYE_ROW, WALL_FACE_Z - 1)),
        Some(MaterialId::AIR),
        "air in front of the wall"
    );
    assert_eq!(
        cell(CellPos::new(40, FIELD_ROW + 1, 90)),
        Some(MaterialId::AIR),
        "air above the field"
    );
    assert_eq!(
        cell(CellPos::new(90, TUNNEL_MIN.0.y, 60)),
        Some(MaterialId::AIR),
        "the tunnel is open"
    );
    assert_jitter_material(t, CELL_JITTER_AMPLITUDE);

    // The basalt wall, face-on at 1.5 m.
    let wall_x = (WALL_MIN.0.x + WALL_MAX.0.x + 1) as f32 * CELL_SIZE_M / 2.0;
    let wall_y = (WALL_EYE_ROW as f32 + 0.5) * CELL_SIZE_M;
    let wall_z = WALL_FACE_Z as f32 * CELL_SIZE_M;
    place_camera(
        t,
        Vec3::new(wall_x, wall_y, wall_z - READING_M),
        Vec3::Z,
        Vec3::Y,
    );
    let wall = shoot(t, "basalt-wall");
    assert_cells_vary(
        "basalt wall",
        (Vec3::new(wall_x + 0.01, wall_y, wall_z), Vec3::NEG_Z),
        (IVec3::X, IVec3::Y),
        PATCH_HALF,
        CellPos::new(35, WALL_EYE_ROW, WALL_FACE_Z),
    );

    // The same view without the jitter: the shader's jitter shows.
    // The renderer repeats a still view exactly, so the difference is the
    // jitter's and not frame-to-frame noise.
    set_amplitude(t, 0.0);
    let flat = shoot(t, "basalt-wall-no-jitter");
    assert_jitter_material(t, 0.0);
    let flat_again = shoot(t, "basalt-wall-no-jitter-again");
    assert_eq!(
        e2e::rms(&flat, &flat_again),
        0.0,
        "a still view renders the same frame twice"
    );
    assert!(
        e2e::rms(&wall, &flat) > 0.0,
        "the jittered wall differs from the unjittered one"
    );
    set_amplitude(t, CELL_JITTER_AMPLITUDE);
    assert_jitter_material(t, CELL_JITTER_AMPLITUDE);

    // The topsoil field, looking straight down from 4 m.
    let field_top = (FIELD_ROW + 1) as f32 * CELL_SIZE_M;
    let field_x = (field_mid.0.x as f32 + 0.5) * CELL_SIZE_M;
    let field_z = (field_mid.0.z as f32 + 0.5) * CELL_SIZE_M;
    place_camera(
        t,
        Vec3::new(field_x, field_top + FIELD_EYE_M, field_z),
        Vec3::NEG_Y,
        Vec3::Z,
    );
    shoot(t, "topsoil-field");
    assert_cells_vary(
        "topsoil field",
        (Vec3::new(field_x, field_top, field_z), Vec3::Y),
        (IVec3::X, IVec3::Z),
        PATCH_HALF,
        field_mid,
    );

    // The tunnel from just inside its mouth, looking along it.
    let floor = TUNNEL_MIN.0.y as f32 * CELL_SIZE_M;
    let tunnel_x = (TUNNEL_MIN.0.x + TUNNEL_MAX.0.x + 1) as f32 * CELL_SIZE_M / 2.0;
    let mouth = TUNNEL_MIN.0.z as f32 * CELL_SIZE_M;
    place_camera(
        t,
        Vec3::new(tunnel_x, floor + TUNNEL_EYE_M, mouth + TUNNEL_IN_M),
        Vec3::Z,
        Vec3::Y,
    );
    shoot(t, "tunnel");
    // The tunnel's -X wall face (toward +X), beside the eye.
    let side = TUNNEL_MIN.0.x as f32 * CELL_SIZE_M;
    assert_cells_vary(
        "tunnel wall",
        (
            Vec3::new(side, (tunnel_wall.0.y as f32 + 0.5) * CELL_SIZE_M, 25.25),
            Vec3::X,
        ),
        (IVec3::Z, IVec3::Y),
        TUNNEL_PATCH_HALF,
        tunnel_wall,
    );
}
