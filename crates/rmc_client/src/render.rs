//! Rendering the replica (SAD §11.2, §11.3).
//!
//! [`GameRenderPlugin`] sets up what the chunk meshes are seen with:
//!
//! - One shared [`ChunkMaterial`]: a `StandardMaterial` with a white
//!   `base_color`, so the mesh's vertex colors (material color × AO) are the
//!   surface color. The color alpha carries emissive strength (SAD §11.2);
//!   the material is opaque, so alpha does not affect its surface. When the
//!   app renders (`PbrPlugin` is present) the plugin also adds
//!   [`EmissiveMaterialPlugin`], and every [`ChunkMesh`] entity gets the
//!   shared [`EmissiveChunkMaterial`] (the same StandardMaterial extended to
//!   glow by the color alpha) instead; otherwise it gets [`ChunkMaterial`].
//! - The [`LiquidMaterial`] (SAD-P2 §10): every [`LiquidMesh`] entity, the
//!   chunk's liquid pass, gets this shared `StandardMaterial`. It is white,
//!   so the vertex colors are the liquid's color and the vertex alpha its
//!   opacity; alpha-blended, so it draws after the opaque pass and the
//!   terrain shows through; double-sided, so a surface is seen from below
//!   too. It is created when the first liquid mesh appears, and liquid
//!   meshes cast no shadow.
//! - The [`GameCamera`]: `Camera3d` with `Hdr` and `Bloom::NATURAL`, order 0
//!   (the camera the mesh pipeline meshes nearest to), starting at
//!   [`CameraStart`].
//! - Lighting: a [`Sun`] (directional, with shadows), a high
//!   `GlobalAmbientLight` so caves stay readable, and a [`Lantern`] point
//!   light that moves with the camera, carried behind the eye so a wall
//!   right in front is not blown out.
//! - The horizon (F088): the camera has a linear [`DistanceFog`] whose color
//!   is [`HORIZON_COLOR`], so distant terrain and the bedrock shell fade into
//!   the sky instead of reading as a black wall, and a [`Skybox`] built by
//!   [`sky_cubemap`]: [`HORIZON_COLOR`] at and below the horizon (exactly the
//!   fog color, so fogged terrain meets the sky without a seam), a gradient
//!   up to [`ZENITH_COLOR`], and a sun disc with a glow where the [`Sun`]
//!   shines from. `ClearColor` is the horizon color too; it shows only where
//!   there is no skybox.
//!
//! Without `Assets<StandardMaterial>` (a logic-only app) there is no
//! material, and without `Assets<Image>` no skybox; the camera (with its
//! fog) and lights are spawned all the same, so systems that follow the
//! camera work in both modes.

use bevy::asset::RenderAssetUsages;
use bevy::camera::{Exposure, Hdr, ShadowLodOrigin};
use bevy::light::{CascadeShadowConfigBuilder, NotShadowCaster, Skybox};
use bevy::pbr::{DistanceFog, FogFalloff, PbrPlugin};
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use rmc_world::coords::{CELL_SIZE_M, CellAabb};

use crate::material::{ChunkEmissiveMaterial, EmissiveChunkMaterial, EmissiveMaterialPlugin};
use crate::meshpipe::{ChunkMesh, LiquidMesh, MeshSystems};
use crate::replica::Replica;

/// The sky at and below the horizon, and the fog's color: terrain fades
/// into it with distance.
pub const HORIZON_COLOR: Color = Color::srgb(0.72, 0.81, 0.90);

/// The sky straight up: deeper and bluer than the horizon.
pub const ZENITH_COLOR: Color = Color::srgb(0.33, 0.55, 0.88);

/// What the camera clears to, seen only where there is no skybox: the
/// horizon color.
pub const SKY_COLOR: Color = HORIZON_COLOR;

/// The sky's gradient: at elevation `e` (the sine of the angle above the
/// horizon) the sky is `e^SKY_GRADIENT_POWER` of the way from the horizon
/// color to the zenith color. Under 1, the pale band hugs the horizon.
pub const SKY_GRADIENT_POWER: f32 = 0.5;

/// The sun disc: its angular radius, in degrees, and its linear HDR color
/// (bright enough to bloom). Its edge blends into the sky over
/// `SUN_DISC_EDGE_DEG` on each side of the radius, about three cubemap
/// texels across, so the disc is round rather than a jagged cluster of
/// texels.
pub const SUN_DISC_RADIUS_DEG: f32 = 1.5;
pub const SUN_DISC_EDGE_DEG: f32 = 0.5;
pub const SUN_DISC_COLOR: LinearRgba = LinearRgba::rgb(12.0, 11.0, 8.5);

/// The glow around the sun: `SUN_GLOW_COLOR × cos(angle)^SUN_GLOW_EXPONENT`
/// is added to the sky, linear. The exponent keeps it within about 10° of
/// the sun, so it never reaches the horizon (the sun is 55° up).
pub const SUN_GLOW_COLOR: LinearRgba = LinearRgba::rgb(0.55, 0.45, 0.28);
pub const SUN_GLOW_EXPONENT: i32 = 256;

/// Texels along each side of a [`sky_cubemap`] face: about 0.35° each, so
/// the sun disc is about 8 texels across.
pub const SKY_FACE_PX: u32 = 256;

/// The fog: none up to `FOG_START_M` from the eye (a wall the player faces,
/// a tunnel, a room are untouched), rising linearly to all fog at
/// `FOG_END_M`. In the default 128 m world the shell is 64 m from the
/// spawn, more than half fog; its far corners are nearly all fog.
pub const FOG_START_M: f32 = 24.0;
pub const FOG_END_M: f32 = 96.0;

/// Above the top of the world the fog thins, so the fly camera high up
/// still sees the ground (F095): for every meter the eye is above the top
/// of the bounds, the fog starts and ends this many meters farther away.
/// Over 1, so looking straight down the top of the world is as clear as a
/// wall [`FOG_START_M`] away is from the ground.
pub const FOG_THINNING_PER_M: f32 = 1.5;

/// Ambient light, in cd/m². Bevy's default is 80; this is high on purpose so
/// unlit caves read clearly (SAD §11.3).
pub const AMBIENT_BRIGHTNESS: f32 = 1200.0;

/// The sun's illuminance, in lux.
pub const SUN_ILLUMINANCE: f32 = 8_000.0;

/// The lantern's luminous power, in lumens, and its range in meters.
///
/// Light falls off with the square of the distance, so a lantern at the eye
/// blows out a wall the player stands against (F087: basalt read near-white
/// at 0.3 m with 400,000 lm). The lantern is carried
/// [`LANTERN_OFFSET_M`] behind the eye instead, which caps how much closer
/// a wall in front can get to it, and is dim enough that at 1.5 m it adds
/// about a quarter of the ambient light: close rock is lit at most ~1.4 ×
/// as brightly as rock at 1.5 m, and dark rock stays dark.
pub const LANTERN_LUMENS: f32 = 160_000.0;
pub const LANTERN_RANGE_M: f32 = 24.0;

/// Where the lantern hangs relative to the camera, in the camera's frame
/// (-Z is forward): 2 m behind the eye and 0.3 m above it. It casts no
/// shadows, so the rock behind a player does not block it.
pub const LANTERN_OFFSET_M: Vec3 = Vec3::new(0.0, 0.3, 2.0);

/// Shadows are drawn this far from the camera, in meters.
pub const SHADOW_DISTANCE_M: f32 = 96.0;

/// How far above the top of the world [`CameraStart::overview`] puts the
/// camera, in meters: high enough to see the sky over the bedrock walls at
/// the world's edge.
pub const OVERVIEW_HEIGHT_M: f32 = 8.0;

/// The camera the game renders with.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct GameCamera;

/// The directional light.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Sun;

/// The point light carried with the camera (the player's lantern).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Lantern;

/// The material every chunk mesh is drawn with.
#[derive(Resource, Clone, Debug)]
pub struct ChunkMaterial(pub Handle<StandardMaterial>);

/// The material every liquid mesh is drawn with (see the module docs).
#[derive(Resource, Clone, Debug)]
pub struct LiquidMaterial(pub Handle<StandardMaterial>);

/// Where the game camera starts. Insert before the first update to change
/// it.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct CameraStart(pub Transform);

impl CameraStart {
    /// [`OVERVIEW_HEIGHT_M`] above the top of `bounds`, over the middle of
    /// its -Z edge, looking across it along +Z and 20° down, so the terrain
    /// fills most of the view and the far wall is as far away as it gets.
    /// A placeholder until the player controller (F060) places the camera.
    pub fn overview(bounds: CellAabb) -> Self {
        let min = bounds.min.min_corner_m();
        let max = bounds.max.min_corner_m() + Vec3::splat(CELL_SIZE_M);
        let eye = Vec3::new((min.x + max.x) / 2.0, max.y + OVERVIEW_HEIGHT_M, min.z);
        let pitch = (-20.0f32).to_radians();
        let dir = Vec3::new(0.0, pitch.sin(), pitch.cos());
        Self(Transform::from_translation(eye).looking_to(dir, Vec3::Y))
    }
}

impl Default for CameraStart {
    fn default() -> Self {
        Self::overview(CellAabb::PHASE1_DEFAULT)
    }
}

/// Chunk material, game camera and lighting.
pub struct GameRenderPlugin;

impl Plugin for GameRenderPlugin {
    fn build(&self, app: &mut App) {
        // A rendering app draws chunks with the emissive extension (F058).
        if app.is_plugin_added::<PbrPlugin>() && !app.is_plugin_added::<EmissiveMaterialPlugin>() {
            app.add_plugins(EmissiveMaterialPlugin);
        }
        app.init_resource::<CameraStart>()
            .insert_resource(ClearColor(SKY_COLOR))
            .insert_resource(GlobalAmbientLight {
                color: Color::WHITE,
                brightness: AMBIENT_BRIGHTNESS,
                affects_lightmapped_meshes: true,
            })
            .add_systems(Startup, (create_chunk_material, spawn_camera, spawn_sun))
            .add_systems(
                Update,
                (add_chunk_material, add_liquid_material).after(MeshSystems),
            )
            .add_systems(PostUpdate, fog_follows_height);
    }
}

/// The shared chunk material, if the app has materials.
fn create_chunk_material(
    mut commands: Commands,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
) {
    let Some(mut materials) = materials else {
        return;
    };
    let handle = materials.add(chunk_material());
    commands.insert_resource(ChunkMaterial(handle));
}

/// White and rough: the vertex colors are the surface color.
pub(crate) fn chunk_material() -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.9,
        metallic: 0.0,
        reflectance: 0.2,
        alpha_mode: AlphaMode::Opaque,
        ..default()
    }
}

fn spawn_camera(
    mut commands: Commands,
    start: Res<CameraStart>,
    images: Option<ResMut<Assets<Image>>>,
) {
    let mut camera = commands.spawn((
        GameCamera,
        Camera3d::default(),
        Camera {
            order: 0,
            ..default()
        },
        Hdr,
        Bloom::NATURAL,
        // Shadow cascades follow this camera, whatever it renders into.
        ShadowLodOrigin,
        start.0,
        children![(
            Lantern,
            PointLight {
                intensity: LANTERN_LUMENS,
                range: LANTERN_RANGE_M,
                shadow_maps_enabled: false,
                ..default()
            },
            Transform::from_translation(LANTERN_OFFSET_M),
        )],
    ));
    camera.insert(distance_fog());
    if let Some(mut images) = images {
        camera.insert(Skybox {
            image: Some(images.add(sky_cubemap())),
            brightness: sky_brightness(),
            rotation: Quat::IDENTITY,
        });
    }
}

fn spawn_sun(mut commands: Commands) {
    commands.spawn((
        Sun,
        DirectionalLight {
            illuminance: SUN_ILLUMINANCE,
            shadow_maps_enabled: true,
            ..default()
        },
        CascadeShadowConfigBuilder {
            num_cascades: 3,
            maximum_distance: SHADOW_DISTANCE_M,
            ..default()
        }
        .build(),
        sun_transform(),
    ));
}

/// The sun's transform: high in the sky (55° up), from the south-west. It
/// shines along its forward axis.
pub fn sun_transform() -> Transform {
    Transform::from_rotation(Quat::from_euler(
        EulerRot::YXZ,
        35f32.to_radians(),
        -55f32.to_radians(),
        0.0,
    ))
}

/// The unit vector from the ground toward the sun.
pub fn sun_direction() -> Vec3 {
    sun_transform().back().as_vec3()
}

/// The fog on the game camera: [`HORIZON_COLOR`], linear from
/// [`FOG_START_M`] to [`FOG_END_M`]. The sun does not tint it, so it
/// matches the sky at the horizon in every direction.
pub fn distance_fog() -> DistanceFog {
    distance_fog_at(0.0)
}

/// The fog for an eye `height_m` above the top of the world bounds: the
/// [`distance_fog`] at or below the top, and above it both ends pushed out
/// by [`FOG_THINNING_PER_M`] per meter.
pub fn distance_fog_at(height_m: f32) -> DistanceFog {
    let push = height_m.max(0.0) * FOG_THINNING_PER_M;
    DistanceFog {
        color: HORIZON_COLOR,
        directional_light_color: Color::NONE,
        directional_light_exponent: 0.0,
        falloff: FogFalloff::Linear {
            start: FOG_START_M + push,
            end: FOG_END_M + push,
        },
    }
}

/// A linear fog's `(start, end)`, in meters; `None` for any other falloff.
pub fn linear_fog_range(falloff: &FogFalloff) -> Option<(f32, f32)> {
    match *falloff {
        FogFalloff::Linear { start, end } => Some((start, end)),
        _ => None,
    }
}

/// Keeps the game camera's fog at [`distance_fog_at`] its height above the
/// top of the replica's bounds. Without a replica the camera keeps the
/// ground-level fog.
fn fog_follows_height(
    replica: Option<Res<Replica>>,
    mut cameras: Query<(&Transform, &mut DistanceFog), With<GameCamera>>,
) {
    let top_m = replica.map(|r| (r.world().bounds().max.0.y + 1) as f32 * CELL_SIZE_M);
    for (transform, mut fog) in &mut cameras {
        let height_m = top_m.map_or(0.0, |top| transform.translation.y - top);
        let want = distance_fog_at(height_m).falloff;
        if linear_fog_range(&fog.falloff) != linear_fog_range(&want) {
            fog.falloff = want;
        }
    }
}

/// How much fog a surface `distance_m` from the eye is under: 0 (none) to
/// 1 (all fog), as Bevy's linear fog computes it.
pub fn fog_amount(distance_m: f32) -> f32 {
    ((distance_m - FOG_START_M) / (FOG_END_M - FOG_START_M)).clamp(0.0, 1.0)
}

/// What a sky is made of. The default is the fixed sky of a game without
/// a clock: [`HORIZON_COLOR`], [`ZENITH_COLOR`], the glow and the disc at
/// [`sun_direction`]. The day and night cycle (`crate::daynight`) makes
/// one for each time of day, without the disc (the sun is drawn apart).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkyParams {
    pub horizon: LinearRgba,
    pub zenith: LinearRgba,
    /// The unit vector toward the sun.
    pub sun: Vec3,
    pub glow: LinearRgba,
    /// Whether the sun disc is drawn into the sky.
    pub disc: bool,
}

impl Default for SkyParams {
    fn default() -> Self {
        Self {
            horizon: LinearRgba::from(HORIZON_COLOR),
            zenith: LinearRgba::from(ZENITH_COLOR),
            sun: sun_direction(),
            glow: SUN_GLOW_COLOR,
            disc: true,
        }
    }
}

/// The sky seen along `dir` (any length), linear, as it reaches the screen
/// before tonemapping: [`HORIZON_COLOR`] at and below the horizon, rising
/// to [`ZENITH_COLOR`] overhead, with the sun's glow and disc around
/// [`sun_direction`].
pub fn sky_radiance(dir: Vec3) -> LinearRgba {
    sky_radiance_with(dir, &SkyParams::default())
}

/// [`sky_radiance`] for the sky `p`.
pub fn sky_radiance_with(dir: Vec3, p: &SkyParams) -> LinearRgba {
    let dir = dir.normalize_or_zero();
    let t = dir.y.max(0.0).powf(SKY_GRADIENT_POWER);
    let mut sky = p.horizon * (1.0 - t) + p.zenith * t;
    let to_sun = dir.dot(p.sun);
    if to_sun > 0.0 {
        sky += p.glow * to_sun.powi(SUN_GLOW_EXPONENT);
    }
    if p.disc {
        // 1 inside the disc, 0 outside it, smooth across its edge.
        let angle = to_sun.clamp(-1.0, 1.0).acos().to_degrees();
        let edge = ((SUN_DISC_RADIUS_DEG + SUN_DISC_EDGE_DEG - angle) / (2.0 * SUN_DISC_EDGE_DEG))
            .clamp(0.0, 1.0);
        let disc = edge * edge * (3.0 - 2.0 * edge);
        sky = sky * (1.0 - disc) + SUN_DISC_COLOR * disc;
    }
    LinearRgba { alpha: 1.0, ..sky }
}

/// The world direction that texel `(x, y)` of cube face `face` (in wgpu's
/// order +X, -X, +Y, -Y, +Z, -Z) of a `size`-texel cubemap shows through
/// Bevy's skybox, which samples world direction `d` at cube coordinate
/// `(d.x, d.y, -d.z)`.
pub fn cube_texel_direction(face: u32, x: u32, y: u32, size: u32) -> Vec3 {
    let s = (x as f32 + 0.5) / size as f32 * 2.0 - 1.0;
    let t = (y as f32 + 0.5) / size as f32 * 2.0 - 1.0;
    let cube = match face {
        0 => Vec3::new(1.0, -t, -s),
        1 => Vec3::new(-1.0, -t, s),
        2 => Vec3::new(s, 1.0, t),
        3 => Vec3::new(s, -1.0, -t),
        4 => Vec3::new(s, -t, 1.0),
        _ => Vec3::new(-s, -t, -1.0),
    };
    Vec3::new(cube.x, cube.y, -cube.z).normalize()
}

/// The sky as a cubemap of [`SKY_FACE_PX`]² faces: [`sky_radiance`] at each
/// texel's direction, in `Rgba16Float` so the sun disc can be brighter than
/// white. Shown with [`sky_brightness`], it reaches the screen as
/// [`sky_radiance`].
pub fn sky_cubemap() -> Image {
    sky_cubemap_with(&SkyParams::default(), SKY_FACE_PX)
}

/// The sky `p` as a cubemap of `size`² faces (see [`sky_cubemap`]).
pub fn sky_cubemap_with(p: &SkyParams, size: u32) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 6,
        },
        TextureDimension::D2,
        &[0; 8],
        TextureFormat::Rgba16Float,
        RenderAssetUsages::default(),
    );
    for face in 0..6 {
        for y in 0..size {
            for x in 0..size {
                let color = sky_radiance_with(cube_texel_direction(face, x, y, size), p);
                image
                    .set_color_at_3d(x, y, face, color.into())
                    .expect("an Rgba16Float texel inside the cubemap");
            }
        }
    }
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });
    image
}

/// The skybox brightness, in cd/m², that undoes the camera's default
/// exposure, so a texel reaches the screen with its own linear value (as
/// the fog color and the clear color do).
pub fn sky_brightness() -> f32 {
    1.0 / Exposure::default().exposure()
}

/// The illuminance, in lux, that `light` at `light_pos` lights a surface at
/// `point` facing along `normal` with, as Bevy's clustered point light
/// computes it: luminous intensity (lumens / 4π) over the squared distance,
/// times the smooth range falloff, times N·L.
pub fn point_light_lux(light: &PointLight, light_pos: Vec3, point: Vec3, normal: Vec3) -> f32 {
    let to_light = light_pos - point;
    let distance_sq = to_light.length_squared().max(1e-4);
    let falloff = (1.0 - (distance_sq / (light.range * light.range)).powi(2))
        .clamp(0.0, 1.0)
        .powi(2);
    let n_dot_l = normal.normalize().dot(to_light.normalize()).max(0.0);
    light.intensity / (4.0 * std::f32::consts::PI) * falloff / distance_sq * n_dot_l
}

/// The illuminance, in lux, that a light would need to make a surface as
/// bright as ambient light of `brightness` (cd/m²) does: Bevy adds
/// albedo × brightness for ambient light and albedo × E / π for a light.
pub fn ambient_equivalent_lux(brightness: f32) -> f32 {
    brightness * std::f32::consts::PI
}

/// Chunk meshes that have neither chunk material yet.
type WithoutChunkMaterial = (
    With<ChunkMesh>,
    Without<MeshMaterial3d<StandardMaterial>>,
    Without<MeshMaterial3d<ChunkEmissiveMaterial>>,
);

/// Gives every chunk mesh without a material the shared one: the emissive
/// material when there is one, otherwise the plain [`ChunkMaterial`].
fn add_chunk_material(
    mut commands: Commands,
    material: Option<Res<ChunkMaterial>>,
    emissive: Option<Res<EmissiveChunkMaterial>>,
    chunks: Query<Entity, WithoutChunkMaterial>,
) {
    if let Some(emissive) = emissive {
        for entity in &chunks {
            commands
                .entity(entity)
                .insert(MeshMaterial3d(emissive.0.clone()));
        }
    } else if let Some(material) = material {
        for entity in &chunks {
            commands
                .entity(entity)
                .insert(MeshMaterial3d(material.0.clone()));
        }
    }
}

/// White, smooth, alpha-blended and double-sided: the vertex colors are
/// the liquid's color, the vertex alpha its opacity.
pub(crate) fn liquid_material() -> StandardMaterial {
    StandardMaterial {
        base_color: Color::WHITE,
        perceptual_roughness: 0.1,
        metallic: 0.0,
        reflectance: 0.5,
        alpha_mode: AlphaMode::Blend,
        double_sided: true,
        cull_mode: None,
        ..default()
    }
}

/// Liquid meshes that have no material yet.
type WithoutLiquidMaterial = (With<LiquidMesh>, Without<MeshMaterial3d<StandardMaterial>>);

/// Gives every liquid mesh without a material the shared
/// [`LiquidMaterial`], creating it on the first one, and keeps liquid
/// meshes out of the shadow maps. Without materials (a logic-only app)
/// nothing is added.
fn add_liquid_material(
    mut commands: Commands,
    liquid: Option<Res<LiquidMaterial>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
    meshes: Query<Entity, WithoutLiquidMaterial>,
) {
    if meshes.is_empty() {
        return;
    }
    let handle = match (liquid, materials) {
        (Some(liquid), _) => liquid.0.clone(),
        (None, Some(mut materials)) => {
            let handle = materials.add(liquid_material());
            commands.insert_resource(LiquidMaterial(handle.clone()));
            handle
        }
        (None, None) => return,
    };
    for entity in &meshes {
        commands
            .entity(entity)
            .insert((MeshMaterial3d(handle.clone()), NotShadowCaster));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesher::{ChunkMesher, GreedyMesher, MeshInput};
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, CellAabb, CellPos, ChunkPos, LocalPos};
    use rmc_world::materials::{MaterialId, MaterialTable};
    use rmc_world::world::World as CellWorld;
    use std::sync::Arc;

    fn render_app(materials: bool) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        if materials {
            app.insert_resource(Assets::<StandardMaterial>::default());
        }
        app.add_plugins(GameRenderPlugin);
        app.update();
        app
    }

    fn spawn_chunk(app: &mut App, x: i32) -> Entity {
        app.world_mut()
            .spawn((
                ChunkMesh {
                    pos: ChunkPos::new(x, 0, 0),
                    revision: 1,
                    quads: 6,
                },
                Transform::default(),
            ))
            .id()
    }

    #[test]
    fn chunk_meshes_get_shared_white_material() {
        let mut app = render_app(true);
        let a = spawn_chunk(&mut app, 0);
        let b = spawn_chunk(&mut app, 1);
        app.update();

        let handle = app.world().resource::<ChunkMaterial>().0.clone();
        for e in [a, b] {
            let m = app
                .world()
                .get::<MeshMaterial3d<StandardMaterial>>(e)
                .expect("chunk mesh has the chunk material");
            assert_eq!(m.0, handle, "one shared material");
        }
        let materials = app.world().resource::<Assets<StandardMaterial>>();
        assert_eq!(materials.len(), 1);
        let mat = materials.get(&handle).expect("material asset");
        assert_eq!(mat.base_color, Color::WHITE, "vertex colors multiply white");
        assert!(!mat.unlit, "lit");
        assert_eq!(
            mat.alpha_mode,
            AlphaMode::Opaque,
            "alpha is emissive, not blend"
        );

        // A chunk spawned later gets it too; one that has it keeps it.
        let c = spawn_chunk(&mut app, 2);
        app.update();
        assert!(
            app.world()
                .get::<MeshMaterial3d<StandardMaterial>>(c)
                .is_some()
        );
        assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 1);

        // Without materials (logic-only) nothing is added and nothing fails.
        let mut logic = render_app(false);
        let d = spawn_chunk(&mut logic, 0);
        logic.update();
        assert!(!logic.world().contains_resource::<ChunkMaterial>());
        assert!(
            logic
                .world()
                .get::<MeshMaterial3d<StandardMaterial>>(d)
                .is_none()
        );
    }

    fn spawn_liquid(app: &mut App, x: i32) -> Entity {
        app.world_mut()
            .spawn((
                LiquidMesh {
                    pos: ChunkPos::new(x, 0, 0),
                    revision: 1,
                    quads: 1,
                },
                Transform::default(),
            ))
            .id()
    }

    #[test]
    fn liquid_meshes_get_shared_blended_material() {
        let mut app = render_app(true);
        let chunk = spawn_chunk(&mut app, 0);
        app.update();
        assert!(
            !app.world().contains_resource::<LiquidMaterial>(),
            "no liquid material before the first liquid mesh"
        );
        let a = spawn_liquid(&mut app, 0);
        let b = spawn_liquid(&mut app, 1);
        app.update();

        let handle = app.world().resource::<LiquidMaterial>().0.clone();
        let opaque = app.world().resource::<ChunkMaterial>().0.clone();
        assert_ne!(handle, opaque, "a second material");
        for e in [a, b] {
            let m = app
                .world()
                .get::<MeshMaterial3d<StandardMaterial>>(e)
                .expect("liquid mesh has the liquid material");
            assert_eq!(m.0, handle, "one shared liquid material");
            assert!(app.world().get::<NotShadowCaster>(e).is_some());
        }
        let on_chunk = app.world().get::<MeshMaterial3d<StandardMaterial>>(chunk);
        assert_eq!(on_chunk.unwrap().0, opaque, "the opaque mesh keeps its own");
        let materials = app.world().resource::<Assets<StandardMaterial>>();
        assert_eq!(materials.len(), 2);
        let mat = materials.get(&handle).expect("material asset");
        assert_eq!(mat.base_color, Color::WHITE, "vertex colors multiply white");
        assert_eq!(
            mat.alpha_mode,
            AlphaMode::Blend,
            "alpha-blended after opaque"
        );
        assert!(
            mat.double_sided && mat.cull_mode.is_none(),
            "seen from below"
        );
        assert!(!mat.unlit, "lit");

        // A later liquid mesh reuses it.
        let c = spawn_liquid(&mut app, 2);
        app.update();
        let m = app.world().get::<MeshMaterial3d<StandardMaterial>>(c);
        assert_eq!(m.unwrap().0, handle);
        assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 2);

        // Logic-only: nothing is added and nothing fails.
        let mut logic = render_app(false);
        let d = spawn_liquid(&mut logic, 0);
        logic.update();
        assert!(!logic.world().contains_resource::<LiquidMaterial>());
        assert!(
            logic
                .world()
                .get::<MeshMaterial3d<StandardMaterial>>(d)
                .is_none()
        );
    }

    #[test]
    fn camera_has_hdr_bloom_and_lantern_and_the_sun_casts_shadows() {
        let start = CameraStart::overview(CellAabb::new(
            CellPos::new(0, 0, 0),
            CellPos::new(63, 31, 127),
        ));
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(start)
            .add_plugins(GameRenderPlugin);
        app.update();
        let world = app.world_mut();

        let (camera, entity, transform, bloom, children) = world
            .query_filtered::<(&Camera, Entity, &Transform, &Bloom, &Children), (
                With<GameCamera>,
                With<Camera3d>,
                With<Hdr>,
            )>()
            .single(world)
            .map(|(c, e, t, b, ch)| (c.clone(), e, *t, b.clone(), ch.to_vec()))
            .expect("one game camera with Hdr and Bloom");
        assert_eq!(camera.order, 0);
        assert!(camera.is_active);
        assert_eq!(bloom.intensity, Bloom::NATURAL.intensity);
        assert_eq!(bloom.composite_mode, Bloom::NATURAL.composite_mode);
        // Above the middle of the -Z edge of a 32 × 16 × 64 m box, looking
        // along +Z and down.
        assert_eq!(
            transform.translation,
            Vec3::new(16.0, 16.0 + OVERVIEW_HEIGHT_M, 0.0)
        );
        assert!(world.get::<ShadowLodOrigin>(entity).is_some());
        let forward = transform.forward();
        assert!(forward.z > 0.9 && forward.y < -0.3 && forward.x.abs() < 1e-5);

        let lanterns: Vec<(Entity, PointLight)> = world
            .query_filtered::<(Entity, &PointLight), With<Lantern>>()
            .iter(world)
            .map(|(e, l)| (e, *l))
            .collect();
        assert_eq!(lanterns.len(), 1, "one lantern");
        let (lantern, light) = lanterns[0];
        assert!(
            children.contains(&lantern),
            "the lantern moves with the camera"
        );
        assert_eq!(
            world.get::<ChildOf>(lantern).map(|c| c.parent()),
            Some(entity)
        );
        assert_eq!(light.intensity, LANTERN_LUMENS);
        assert_eq!(light.range, LANTERN_RANGE_M);

        let suns: Vec<DirectionalLight> = world
            .query_filtered::<&DirectionalLight, With<Sun>>()
            .iter(world)
            .cloned()
            .collect();
        assert_eq!(suns.len(), 1, "one sun");
        assert!(suns[0].shadow_maps_enabled, "the sun casts shadows");
        assert_eq!(suns[0].illuminance, SUN_ILLUMINANCE);

        let ambient = world.resource::<GlobalAmbientLight>();
        assert!(ambient.brightness > GlobalAmbientLight::default().brightness * 10.0);
        assert_eq!(world.resource::<ClearColor>().0, SKY_COLOR);
    }

    #[test]
    fn lantern_is_soft_at_close_range() {
        let mut app = render_app(false);
        let world = app.world_mut();
        let camera = world
            .query_filtered::<&Transform, With<GameCamera>>()
            .single(world)
            .copied()
            .expect("one game camera");
        let (light, local) = world
            .query_filtered::<(&PointLight, &Transform), With<Lantern>>()
            .single(world)
            .map(|(l, t)| (*l, *t))
            .expect("one lantern");
        let light_pos = camera.mul_transform(local).translation;
        let forward = camera.forward().as_vec3();
        let ambient = ambient_equivalent_lux(world.resource::<GlobalAmbientLight>().brightness);

        // A wall facing the camera `d` meters in front of it: what the
        // lantern and the ambient light light it with.
        let lantern = |d: f32| {
            point_light_lux(
                &light,
                light_pos,
                camera.translation + forward * d,
                -forward,
            )
        };
        let total = |d: f32| ambient + lantern(d);

        // From the near plane (0.1 m) to the reading distance, the wall is
        // never more than MAX_CLOSE_GAIN times as lit as at 1.5 m. (1.5 in
        // linear light is about 1.2 in sRGB bytes, under the 1.25 the
        // lantern_close_wall scenario allows.)
        const MAX_CLOSE_GAIN: f32 = 1.5;
        let reading = total(1.5);
        for cm in 10..=150u16 {
            let d = f32::from(cm) / 100.0;
            assert!(
                total(d) <= MAX_CLOSE_GAIN * reading,
                "at {d} m the wall gets {:.0} lx, over {MAX_CLOSE_GAIN} × the {reading:.0} lx at 1.5 m",
                total(d)
            );
        }

        // It is still a lantern: at the reading distance it adds at least a
        // quarter of the ambient light, it fades with distance, and nothing
        // past its range.
        assert!(
            lantern(1.5) >= ambient / 4.0,
            "the lantern adds {:.0} lx at 1.5 m, ambient is {ambient:.0} lx",
            lantern(1.5)
        );
        assert!(lantern(1.5) > lantern(4.0) && lantern(4.0) > lantern(10.0));
        assert_eq!(lantern(LANTERN_RANGE_M + 1.0), 0.0);
        // Up close the light comes from behind the eye, not from the eye.
        assert!(
            (light_pos - camera.translation).dot(forward) < 0.0,
            "the lantern is carried behind the camera"
        );

        // The formula: a 4π-lumen bare light 1 m away lights a facing
        // surface with 1 lx, 2 m away with 1/4 lx, and a surface turned
        // away with nothing.
        let unit = PointLight {
            intensity: 4.0 * std::f32::consts::PI,
            range: 100.0,
            ..default()
        };
        let lux = |p: Vec3, n: Vec3| point_light_lux(&unit, Vec3::ZERO, p, n);
        assert!((lux(Vec3::X, Vec3::NEG_X) - 1.0).abs() < 1e-3);
        assert!((lux(Vec3::X * 2.0, Vec3::NEG_X) - 0.25).abs() < 1e-3);
        assert_eq!(lux(Vec3::X, Vec3::X), 0.0);
    }
    #[test]
    fn camera_has_distance_fog_matching_sky() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Assets::<Image>::default())
            .add_plugins(GameRenderPlugin);
        app.update();
        let world = app.world_mut();
        let (fog, skybox, exposure) = world
            .query_filtered::<(&DistanceFog, Option<&Skybox>, Option<&Exposure>), With<GameCamera>>(
            )
            .single(world)
            .map(|(f, s, e)| (f.clone(), s.cloned(), e.copied()))
            .expect("the game camera has distance fog");
        let sun = world
            .query_filtered::<&Transform, With<Sun>>()
            .single(world)
            .copied()
            .expect("one sun");
        let to_sun = sun.back().as_vec3();

        // The fog is the horizon color, untinted by the sun, and it is the
        // clear color too.
        assert_eq!(fog.color, HORIZON_COLOR);
        assert_eq!(fog.directional_light_color.alpha(), 0.0, "no sun tint");
        assert_eq!(world.resource::<ClearColor>().0, HORIZON_COLOR);
        let FogFalloff::Linear { start, end } = fog.falloff else {
            panic!("linear fog, got {:?}", fog.falloff);
        };
        assert_eq!((start, end), (FOG_START_M, FOG_END_M));
        // Nothing near the player is fogged: a wall, a tunnel, a room.
        assert!(start >= 16.0, "fog starts at {start} m");
        assert_eq!(fog_amount(start), 0.0);
        assert_eq!(fog_amount(end), 1.0);
        // In the default world the shell is 64 m from the spawn in the
        // middle: at least half fog there, but still a shape.
        let bounds = CellAabb::PHASE1_DEFAULT;
        let half_width_m = (bounds.max.0.x - bounds.min.0.x + 1) as f32 * CELL_SIZE_M / 2.0;
        let shell = fog_amount(half_width_m);
        assert!(
            (0.5..1.0).contains(&shell),
            "the shell {half_width_m} m away is {shell} fog"
        );

        // The sky meets the fog at the horizon in every direction, below
        // it too, and is bluer and deeper overhead.
        let fog_linear = LinearRgba::from(fog.color);
        let close = |a: LinearRgba, b: LinearRgba, eps: f32| {
            (a.red - b.red).abs() <= eps
                && (a.green - b.green).abs() <= eps
                && (a.blue - b.blue).abs() <= eps
        };
        for deg in 0..360 {
            let a = (deg as f32).to_radians();
            for y in [0.0, -0.3, -1.0] {
                let dir = Vec3::new(a.cos(), y, a.sin());
                assert!(
                    close(sky_radiance(dir), fog_linear, 1e-6),
                    "the sky at azimuth {deg}°, y {y} is {:?}, the fog {fog_linear:?}",
                    sky_radiance(dir)
                );
            }
        }
        let zenith = sky_radiance(Vec3::Y);
        assert!(zenith.luminance() < fog_linear.luminance() * 0.8);
        assert!(zenith.blue / zenith.red > fog_linear.blue / fog_linear.red * 1.5);
        // The gradient rises steadily from the horizon up.
        let away = Vec3::new(-to_sun.x, 0.0, -to_sun.z).normalize();
        let mut last = fog_linear.luminance();
        for step in 1..=9 {
            let e = (step as f32 * 10.0).to_radians();
            let lum = sky_radiance(away * e.cos() + Vec3::Y * e.sin()).luminance();
            assert!(
                lum < last,
                "the sky darkens toward the zenith at {}°",
                step * 10
            );
            last = lum;
        }

        // The sun's disc is where the Sun shines from, and far brighter
        // than white, so it blooms.
        assert!((to_sun - sun_direction()).length() < 1e-6);
        assert!(to_sun.y > 0.5, "the sun is high: {to_sun:?}");
        let disc = sky_radiance(to_sun);
        assert!(
            disc.red > 4.0 && disc.green > 4.0 && disc.blue > 4.0,
            "{disc:?}"
        );
        let glow = sky_radiance((to_sun + Vec3::Y * 0.06).normalize());
        let plain = sky_radiance(Vec3::new(-to_sun.x, to_sun.y + 0.06, -to_sun.z).normalize());
        assert!(
            glow.luminance() > plain.luminance() * 1.2,
            "a glow around the sun"
        );

        // The skybox is that sky: a cubemap whose texels hold
        // sky_radiance for their direction, shown at a brightness that
        // undoes the camera's exposure.
        let skybox = skybox.expect("a skybox when the app has images");
        let exposure = exposure.unwrap_or_default().exposure();
        assert!(
            (skybox.brightness * exposure - 1.0).abs() < 1e-4,
            "{} cd/m² at exposure {exposure}",
            skybox.brightness
        );
        let images = world.resource::<Assets<Image>>();
        let image = images
            .get(skybox.image.as_ref().expect("a skybox image"))
            .expect("the skybox image asset");
        assert_eq!(image.texture_descriptor.size.depth_or_array_layers, 6);
        assert_eq!(
            image
                .texture_view_descriptor
                .as_ref()
                .and_then(|v| v.dimension),
            Some(TextureViewDimension::Cube)
        );
        let size = image.texture_descriptor.size.width;
        assert_eq!(size, SKY_FACE_PX);
        let mut brightest = (0.0f32, Vec3::ZERO);
        for face in 0..6 {
            for y in 0..size {
                for x in 0..size {
                    let dir = cube_texel_direction(face, x, y, size);
                    let texel = LinearRgba::from(image.get_color_at_3d(x, y, face).unwrap());
                    // f16 keeps about 3 decimal digits.
                    let want = sky_radiance(dir);
                    assert!(
                        close(texel, want, want.red.max(want.blue) * 2e-3),
                        "texel {face}/{x}/{y} {texel:?}, sky {want:?}"
                    );
                    if dir.y <= 0.0 {
                        assert!(close(texel, fog_linear, 2e-3), "below the horizon");
                    }
                    if texel.luminance() > brightest.0 {
                        brightest = (texel.luminance(), dir);
                    }
                }
            }
        }
        assert!(
            brightest.1.angle_between(to_sun).to_degrees()
                <= SUN_DISC_RADIUS_DEG - SUN_DISC_EDGE_DEG,
            "the brightest texel looks along {:?}, the sun is along {to_sun:?}",
            brightest.1
        );
        // Each face's texels look the way that face points.
        let axes = [
            Vec3::X,
            Vec3::NEG_X,
            Vec3::Y,
            Vec3::NEG_Y,
            Vec3::NEG_Z,
            Vec3::Z,
        ];
        for (face, axis) in (0..6).zip(axes) {
            let mid = cube_texel_direction(face, size / 2, size / 2, size);
            assert!(mid.dot(axis) > 0.99, "face {face} looks along {mid:?}");
        }

        // A logic-only app (no images): fog, but no skybox, and no failure.
        let mut logic = render_app(false);
        let world = logic.world_mut();
        let (fog, skybox) = world
            .query_filtered::<(&DistanceFog, Option<&Skybox>), With<GameCamera>>()
            .single(world)
            .map(|(f, s)| (f.color, s.is_some()))
            .expect("fog without images");
        assert_eq!(fog, HORIZON_COLOR);
        assert!(!skybox);
    }

    #[test]
    fn fog_thins_with_camera_height() {
        // At or below the top of the world: the ground-level fog.
        for h in [-80.0, -1.0, 0.0] {
            assert_eq!(
                linear_fog_range(&distance_fog_at(h).falloff),
                Some((FOG_START_M, FOG_END_M)),
                "{h} m"
            );
        }
        // Above it, the fog starts and ends farther away the higher the
        // eye, keeps its color, and the top of the world straight below is
        // never more fogged than a wall FOG_START_M away on the ground.
        let mut last = (FOG_START_M, FOG_END_M);
        for h in [1.0, 8.0, 24.0, crate::flycam::FLY_BOUNDS_MARGIN_M] {
            let fog = distance_fog_at(h);
            let (start, end) = linear_fog_range(&fog.falloff).expect("linear fog");
            assert!(start > last.0 && end > last.1, "{h} m: {start}..{end}");
            assert!(start - h >= FOG_START_M, "{h} m: the ground below is clear");
            assert_eq!(end - start, FOG_END_M - FOG_START_M, "the same band");
            assert_eq!(fog.color, HORIZON_COLOR);
            last = (start, end);
        }

        // The game camera's fog follows its height over the replica's top
        // (16 m here).
        let table = Arc::new(MaterialTable::load(MATERIALS_PATH).expect("materials load"));
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 31, 63));
        let top_m = 32.0 * CELL_SIZE_M;
        let mut app = render_app(false);
        let camera_fog = |app: &mut App, y: f32| {
            let world = app.world_mut();
            let mut q = world.query_filtered::<&mut Transform, With<GameCamera>>();
            q.single_mut(world).expect("one game camera").translation.y = y;
            app.update();
            let world = app.world_mut();
            let mut q = world.query_filtered::<&DistanceFog, With<GameCamera>>();
            linear_fog_range(&q.single(world).expect("fog").falloff)
        };
        // No replica yet: ground-level fog, however high.
        assert_eq!(camera_fog(&mut app, 500.0), Some((FOG_START_M, FOG_END_M)));
        app.insert_resource(Replica::from_world(CellWorld::new(table, bounds)));
        for h in [30.0, 5.0, -3.0, 0.0, 12.5] {
            assert_eq!(
                camera_fog(&mut app, top_m + h),
                linear_fog_range(&distance_fog_at(h).falloff),
                "{h} m above the top"
            );
        }
    }

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    /// The ground's top in [`shell_world`], in cells: stone below it.
    const SURFACE_Y: i32 = 40;

    /// 64³ cells (2 × 2 × 2 chunks) shaped like a generated world: a bedrock
    /// shell (two floor layers, one-cell walls the full height of the
    /// bounds), stone up to [`SURFACE_Y`], air above. A cave touches the -X
    /// wall underground, and a stone slab at y 50 juts from it above the
    /// ground.
    fn shell_world() -> CellWorld {
        let table = Arc::new(MaterialTable::load(MATERIALS_PATH).expect("materials load"));
        let bedrock = table.id("bedrock").unwrap();
        let stone = table.id("stone").unwrap();
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(63, 63, 63));
        let (lo, hi) = (bounds.min.0, bounds.max.0);
        let in_box = |c: IVec3, a: IVec3, b: IVec3| c.cmpge(a).all() && c.cmple(b).all();
        let material = |c: IVec3| {
            let wall = c.x == lo.x || c.x == hi.x || c.z == lo.z || c.z == hi.z;
            if c.y - lo.y < 2 || wall {
                bedrock
            } else if in_box(c, IVec3::new(1, 10, 20), IVec3::new(3, 12, 22)) {
                MaterialId::AIR
            } else if c.y < SURFACE_Y || in_box(c, IVec3::new(1, 50, 30), IVec3::new(5, 50, 34)) {
                stone
            } else {
                MaterialId::AIR
            }
        };
        let mut world = CellWorld::new(table, bounds);
        for pos in bounds.chunks() {
            let mut chunk = Chunk::uniform(MaterialId::AIR);
            let origin = pos.origin_cell().0;
            for i in 0..CHUNK_VOLUME {
                let local = LocalPos::from_index(i as u16);
                chunk
                    .storage
                    .set(local, material(origin + local.0.as_ivec3()));
            }
            world.insert_chunk(pos, chunk).expect("chunk in bounds");
        }
        world
    }

    /// One meshed quad in world cells: its normal and its corners.
    struct WorldQuad {
        normal: IVec3,
        corners: [Vec3; 4],
    }

    impl WorldQuad {
        /// Whether the quad faces along `normal` and covers `point` (cells).
        fn covers(&self, normal: IVec3, point: Vec3) -> bool {
            let axis = (0..3).find(|&a| normal[a] != 0).unwrap();
            let min = self.corners.iter().fold(Vec3::MAX, |m, &c| m.min(c));
            let max = self.corners.iter().fold(Vec3::MIN, |m, &c| m.max(c));
            self.normal == normal
                && (min[axis] - point[axis]).abs() < 1e-4
                && (0..3)
                    .filter(|&a| a != axis)
                    .all(|a| min[a] <= point[a] && point[a] <= max[a])
        }
    }

    /// Every quad the greedy mesher makes for `world`, in world cells.
    fn world_quads(world: &CellWorld) -> Vec<WorldQuad> {
        let mut out = Vec::new();
        for pos in world.bounds().chunks() {
            let input = MeshInput::from_world(world, pos).expect("resident chunk");
            let data = GreedyMesher.mesh(&input);
            let origin = pos.origin_cell().0.as_vec3();
            for q in 0..data.quad_count as usize {
                let n = data.normals[4 * q];
                out.push(WorldQuad {
                    normal: IVec3::new(n[0] as i32, n[1] as i32, n[2] as i32),
                    corners: std::array::from_fn(|k| {
                        origin + Vec3::from(data.positions[4 * q + k]) / CELL_SIZE_M
                    }),
                });
            }
        }
        out
    }

    #[test]
    fn shell_above_surface_is_not_drawn_as_a_wall() {
        let world = shell_world();
        let quads = world_quads(&world);
        assert!(!quads.is_empty());

        // Above the ground nothing is drawn but the slab and the shell it
        // roofs: no wall stands over the terrain at the world's edge, from
        // inside (its inward faces), from above (its top) or from outside.
        let overhang = |c: Vec3| c.x <= 6.0 && c.y <= 51.0 && (30.0..=35.0).contains(&c.z);
        for q in &quads {
            if q.corners.iter().any(|c| c.y > SURFACE_Y as f32) {
                assert!(
                    q.corners.iter().all(|&c| overhang(c)),
                    "a quad above the surface, away from the slab: normal {}, corners {:?}",
                    q.normal,
                    q.corners
                );
            }
        }

        // The shell is drawn where the sky is not open above it, so the
        // world is closed underground and under a roof.
        let has = |normal: IVec3, point: Vec3| quads.iter().any(|q| q.covers(normal, point));
        assert!(
            has(IVec3::X, Vec3::new(1.0, 11.5, 21.5)),
            "the wall beside the cave"
        );
        assert!(
            has(IVec3::X, Vec3::new(1.0, 45.5, 32.5)),
            "the wall under the slab"
        );
        assert!(
            has(IVec3::NEG_X, Vec3::new(0.0, 20.5, 10.5)),
            "the shell's outer face below the ground"
        );
        assert!(
            has(IVec3::NEG_Y, Vec3::new(10.5, 0.0, 10.5)),
            "the floor's underside"
        );
        // The wall's top at the ground is drawn, so the edge has no gap.
        for point in [
            Vec3::new(0.5, SURFACE_Y as f32, 10.5),
            Vec3::new(63.5, SURFACE_Y as f32, 10.5),
            Vec3::new(10.5, SURFACE_Y as f32, 0.5),
            Vec3::new(10.5, SURFACE_Y as f32, 63.5),
            Vec3::new(0.5, SURFACE_Y as f32, 0.5),
        ] {
            assert!(has(IVec3::Y, point), "the shell's top at {point}");
        }

        // Only the mesh changes: the replica keeps the wall, so the player
        // still stops at the world's edge.
        let table = world.materials().clone();
        let bedrock = table.id("bedrock").unwrap();
        for y in SURFACE_Y..64 {
            assert_eq!(world.get_cell(CellPos::new(0, y, 10)), Some(bedrock));
        }
    }
}
