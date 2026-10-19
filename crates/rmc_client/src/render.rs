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
//! - The [`GameCamera`]: `Camera3d` with `Hdr` and `Bloom::NATURAL`, order 0
//!   (the camera the mesh pipeline meshes nearest to), starting at
//!   [`CameraStart`].
//! - Lighting: a [`Sun`] (directional, with shadows), a high
//!   `GlobalAmbientLight` so caves stay readable, and a [`Lantern`] point
//!   light that moves with the camera, carried behind the eye so a wall
//!   right in front is not blown out, and a sky-blue `ClearColor`.
//!
//! Without `Assets<StandardMaterial>` (a logic-only app) there is no
//! material; the camera and lights are spawned all the same, so systems that
//! follow the camera work in both modes.

use bevy::camera::{Hdr, ShadowLodOrigin};
use bevy::light::CascadeShadowConfigBuilder;
use bevy::pbr::PbrPlugin;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;
use rmc_world::coords::{CELL_SIZE_M, CellAabb};

use crate::material::{ChunkEmissiveMaterial, EmissiveChunkMaterial, EmissiveMaterialPlugin};
use crate::meshpipe::{ChunkMesh, MeshSystems};

/// The sky: what the camera clears to.
pub const SKY_COLOR: Color = Color::srgb(0.53, 0.72, 0.92);

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
            .add_systems(Update, add_chunk_material.after(MeshSystems));
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

fn spawn_camera(mut commands: Commands, start: Res<CameraStart>) {
    commands.spawn((
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
        // High in the sky, from the south-west.
        Transform::from_rotation(Quat::from_euler(
            EulerRot::YXZ,
            35f32.to_radians(),
            -55f32.to_radians(),
            0.0,
        )),
    ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use rmc_world::coords::{CellAabb, CellPos, ChunkPos};

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
}
