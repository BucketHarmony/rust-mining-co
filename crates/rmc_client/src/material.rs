//! Emissive chunk material (SAD §11.2).
//!
//! The mesher stores each vertex's emissive strength in its color alpha.
//! [`ChunkEmissiveMaterial`] is the chunk [`StandardMaterial`] extended with
//! [`EmissiveFromAlpha`], whose fragment shader (`emissive.wesl`, embedded
//! as [`EMISSIVE_SHADER_PATH`]) adds `color.rgb × color.a × emissive_scale`
//! to the emissive term, so glowing cells glow in their own color and feed
//! the camera's bloom. It also scales each cell's color by
//! [`cell_jitter`] (F097), a few percent from a hash of the cell, so large
//! faces read as rock rather than flat paint. Everything else is shaded as
//! by the StandardMaterial.
//!
//! Faces of materials with a tile ([`crate::tiles`]) take their surface color
//! from the tile texture instead: the mesher writes each vertex's material
//! id in its first UV, and the shader looks up the material's tile layer for
//! the face direction and scales the texel by the vertex color's face shade
//! × AO.
//!
//! [`EmissiveMaterialPlugin`] registers the material and creates the shared
//! [`EmissiveChunkMaterial`]. `GameRenderPlugin` adds it when the app
//! renders (`PbrPlugin` is present) and then gives chunk meshes this
//! material instead of the plain [`crate::render::ChunkMaterial`].

use bevy::asset::embedded_asset;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::AsBindGroup;
use bevy::shader::ShaderRef;
use rmc_world::coords::CELL_SIZE_M;

use crate::render::chunk_material;
use crate::tiles::{TileLookup, TilesPlugin};

/// Where the extension's fragment shader is embedded.
pub const EMISSIVE_SHADER_PATH: &str = "embedded://rmc_client/emissive.wesl";

/// The fragment shader's source.
pub const EMISSIVE_SHADER: &str = include_str!("emissive.wesl");

/// Emitted luminance per unit of emissive strength, as a multiple of the
/// surface's linear color. Emission is not scaled by the camera exposure
/// (StandardMaterial's default `emissive_exposure_weight` of 0), so 1.0
/// makes a strength-1 cell as bright as a fully lit white surface; the
/// crystal (strength 2) is twice that and blooms.
pub const EMISSIVE_SCALE: f32 = 1.0;

/// Per-cell brightness variation (F097): each cell's surface color is
/// scaled by a factor in `1 ± CELL_JITTER_AMPLITUDE`, from [`cell_jitter`],
/// so a large greedy-merged face reads as cells of rock or soil rather
/// than flat paint. The mesh is unchanged; the fragment shader applies it.
/// The factor scales linear color, and sRGB encoding roughly halves a
/// relative step (gamma ≈ 2.2), so 0.08 is about ±3.5% of perceived
/// brightness: "a few percent" as the player sees it.
pub const CELL_JITTER_AMPLITUDE: f32 = 0.08;

/// How far inside a face, along its normal, the fragment's cell is taken,
/// so a fragment on a face belongs to the solid cell behind it.
pub const CELL_JITTER_INSET_M: f32 = 0.001;

/// The hash's per-axis multipliers (odd, so each axis is a bijection).
pub const CELL_HASH_MULTIPLIERS: [u32; 3] = [0x8da6_b343, 0xd816_3841, 0xcb1a_b31f];

/// The hash's two finalizer multipliers (lowbias32).
pub const CELL_HASH_MIXERS: [u32; 2] = [0x7feb_352d, 0x846c_a68b];

/// The top 24 bits of the hash form the jitter, exactly representable in
/// an `f32`.
const JITTER_BITS_SHIFT: u32 = 8;
const JITTER_BITS_MAX: f32 = 16_777_215.0;

/// A 32-bit hash of a cell's coordinates. `emissive.wesl`'s `cell_hash`
/// is the same function.
pub fn cell_hash(cell: IVec3) -> u32 {
    let [mx, my, mz] = CELL_HASH_MULTIPLIERS;
    let [m1, m2] = CELL_HASH_MIXERS;
    let mut h = (cell.x as u32).wrapping_mul(mx)
        ^ (cell.y as u32).wrapping_mul(my)
        ^ (cell.z as u32).wrapping_mul(mz);
    h ^= h >> 16;
    h = h.wrapping_mul(m1);
    h ^= h >> 15;
    h = h.wrapping_mul(m2);
    h ^= h >> 16;
    h
}

/// The brightness factor of `cell`, in `1 ± CELL_JITTER_AMPLITUDE`.
/// `emissive.wesl`'s `cell_jitter` is the same function.
pub fn cell_jitter(cell: IVec3) -> f32 {
    let unit = (cell_hash(cell) >> JITTER_BITS_SHIFT) as f32 / JITTER_BITS_MAX;
    1.0 + CELL_JITTER_AMPLITUDE * (2.0 * unit - 1.0)
}

/// The cell a fragment at `world_pos` on a face with outward `normal`
/// belongs to: the cell [`CELL_JITTER_INSET_M`] behind it.
pub fn fragment_cell(world_pos: Vec3, normal: Vec3) -> IVec3 {
    ((world_pos - normal.normalize() * CELL_JITTER_INSET_M) / CELL_SIZE_M)
        .floor()
        .as_ivec3()
}

/// Material extension: emission from the vertex color's alpha.
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug, PartialEq)]
pub struct EmissiveFromAlpha {
    /// Multiplies `color.rgb × color.a` before it is added to emission.
    #[uniform(100)]
    pub emissive_scale: f32,
    /// [`CELL_JITTER_AMPLITUDE`].
    #[uniform(101)]
    pub jitter_amplitude: f32,
    /// [`CELL_SIZE_M`].
    #[uniform(102)]
    pub jitter_cell_size_m: f32,
    /// [`CELL_JITTER_INSET_M`].
    #[uniform(103)]
    pub jitter_inset_m: f32,
    /// The tile textures ([`crate::tiles`]); `None` until a tile set loads.
    #[texture(104, dimension = "2d_array")]
    #[sampler(105)]
    pub tiles: Option<Handle<Image>>,
    /// Which tile layer each material's faces use, and its linear color.
    #[uniform(106)]
    pub tile_lookup: TileLookup,
}

impl Default for EmissiveFromAlpha {
    fn default() -> Self {
        Self {
            emissive_scale: EMISSIVE_SCALE,
            jitter_amplitude: CELL_JITTER_AMPLITUDE,
            jitter_cell_size_m: CELL_SIZE_M,
            jitter_inset_m: CELL_JITTER_INSET_M,
            tiles: None,
            tile_lookup: TileLookup::default(),
        }
    }
}

impl MaterialExtension for EmissiveFromAlpha {
    fn fragment_shader() -> ShaderRef {
        EMISSIVE_SHADER_PATH.into()
    }
}

/// The chunk material with emission.
pub type ChunkEmissiveMaterial = ExtendedMaterial<StandardMaterial, EmissiveFromAlpha>;

/// The emissive material every chunk mesh is drawn with when the app
/// renders.
#[derive(Resource, Clone, Debug)]
pub struct EmissiveChunkMaterial(pub Handle<ChunkEmissiveMaterial>);

/// Registers [`ChunkEmissiveMaterial`], creates [`EmissiveChunkMaterial`] and
/// adds the [`TilesPlugin`] that gives it its tiles.
pub struct EmissiveMaterialPlugin;

impl Plugin for EmissiveMaterialPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "emissive.wesl");
        app.add_plugins((
            MaterialPlugin::<ChunkEmissiveMaterial>::default(),
            TilesPlugin,
        ))
        .add_systems(Startup, create_emissive_chunk_material);
    }
}

/// The shared emissive chunk material: the chunk StandardMaterial, extended.
fn create_emissive_chunk_material(
    mut commands: Commands,
    materials: Option<ResMut<Assets<ChunkEmissiveMaterial>>>,
) {
    let Some(mut materials) = materials else {
        return;
    };
    let handle = materials.add(ExtendedMaterial {
        base: chunk_material(),
        extension: EmissiveFromAlpha::default(),
    });
    commands.insert_resource(EmissiveChunkMaterial(handle));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesher::{ChunkMesher, GreedyMesher, MeshInput};
    use crate::meshpipe::ChunkMesh;
    use crate::render::GameRenderPlugin;
    use bevy::asset::{AssetPath, AssetPlugin};
    use bevy::pbr::Material;
    use rmc_world::coords::{CHUNK_SIZE, ChunkPos};
    use rmc_world::materials::MaterialTable;
    use std::sync::Arc;
    use wesl::syntax::{Attribute, GlobalDeclaration, TranslationUnit};

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    fn is_material<M: Material>() {}

    #[test]
    fn cell_jitter_is_deterministic_and_bounded() {
        // A few percent, so walls read as rock without changing their color.
        let ext = EmissiveFromAlpha::default();
        assert!(
            ext.jitter_amplitude > 0.0 && ext.jitter_amplitude <= 0.1,
            "a few percent: {}",
            ext.jitter_amplitude
        );

        // Deterministic, bounded by the amplitude, different between most
        // adjacent cells, and spread over the whole range around 1.
        let (mut lo, mut hi, mut sum, mut n) = (f32::MAX, f32::MIN, 0.0f64, 0u32);
        let (mut pairs, mut differ) = (0u32, 0u32);
        for x in -24..24 {
            for y in -8..72 {
                for z in -24..24 {
                    let c = IVec3::new(x, y, z);
                    let j = cell_jitter(c);
                    assert_eq!(j, cell_jitter(c), "deterministic at {c}");
                    assert!(
                        (j - 1.0).abs() <= CELL_JITTER_AMPLITUDE + 1e-6,
                        "{c}: {j} within 1 ± {CELL_JITTER_AMPLITUDE}"
                    );
                    lo = lo.min(j);
                    hi = hi.max(j);
                    sum += f64::from(j);
                    n += 1;
                    for d in [IVec3::X, IVec3::Y, IVec3::Z] {
                        pairs += 1;
                        differ += u32::from(cell_jitter(c + d) != j);
                    }
                }
            }
        }
        assert!(
            differ * 10 >= pairs * 9,
            "{differ} of {pairs} adjacent pairs differ"
        );
        assert!(
            hi - lo >= CELL_JITTER_AMPLITUDE * 1.9,
            "the range is used: {lo}..{hi}"
        );
        let mean = sum / f64::from(n);
        assert!(
            (mean - 1.0).abs() < f64::from(CELL_JITTER_AMPLITUDE) / 10.0,
            "centered on 1: mean {mean}"
        );

        // The hashed cell is the one behind the face, a hair inside along
        // the normal, at every point of the face up to its edges.
        let s = CELL_SIZE_M;
        for (point, normal, cell) in [
            // The +X face of (9, 4, 2) and the -X face of (10, 4, 2).
            (Vec3::new(10.0, 4.5, 2.5) * s, Vec3::X, IVec3::new(9, 4, 2)),
            (
                Vec3::new(10.0, 4.5, 2.5) * s,
                Vec3::NEG_X,
                IVec3::new(10, 4, 2),
            ),
            // The top of (3, 20, -5) at its low corner, the top of
            // (4, 20, -4) at its own, and a point just short of the latter.
            (
                Vec3::new(3.0, 21.0, -5.0) * s,
                Vec3::Y,
                IVec3::new(3, 20, -5),
            ),
            (
                Vec3::new(4.0, 21.0, -4.0) * s,
                Vec3::Y,
                IVec3::new(4, 20, -4),
            ),
            (
                Vec3::new(3.999, 21.0, -4.001) * s,
                Vec3::Y,
                IVec3::new(3, 20, -5),
            ),
            // The underside of (-7, 0, 0) and the -Z face of (1, 2, 30).
            (
                Vec3::new(-6.5, 0.0, 0.5) * s,
                Vec3::NEG_Y,
                IVec3::new(-7, 0, 0),
            ),
            (
                Vec3::new(1.5, 2.5, 30.0) * s,
                Vec3::NEG_Z,
                IVec3::new(1, 2, 30),
            ),
        ] {
            assert_eq!(
                fragment_cell(point, normal),
                cell,
                "{point} facing {normal}"
            );
        }

        // The shader mirrors the function: the same hash constants, and the
        // amplitude, cell size and inset as the uniforms the derive declares.
        // The factor scales the base color before the glow is taken from
        // it, so emission still works and jitters with its cell.
        for k in CELL_HASH_MULTIPLIERS.iter().chain(&CELL_HASH_MIXERS) {
            let literal = format!("0x{k:08x}u");
            assert!(EMISSIVE_SHADER.contains(&literal), "shader has {literal}");
        }
        let unit: TranslationUnit = EMISSIVE_SHADER
            .parse()
            .unwrap_or_else(|e| panic!("emissive.wesl parses: {e}"));
        let mut uniforms = Vec::new();
        for decl in &unit.global_declarations {
            if let GlobalDeclaration::Declaration(d) = &**decl {
                let binding = d.attributes.iter().find_map(|a| match &**a {
                    Attribute::Binding(e) => Some(e.to_string()),
                    _ => None,
                });
                uniforms.push((d.ident.name().clone(), binding));
            }
        }
        for (name, binding) in [
            ("jitter_amplitude", "101"),
            ("jitter_cell_size_m", "102"),
            ("jitter_inset_m", "103"),
        ] {
            assert!(
                uniforms.contains(&(name.to_string(), Some(binding.to_string()))),
                "{name} at binding {binding}: {uniforms:?}"
            );
        }
        let jitter = EMISSIVE_SHADER
            .find("let shade = cell_jitter(")
            .expect("the fragment computes the cell's factor");
        let glow = EMISSIVE_SHADER.find("let glow").expect("computes the glow");
        assert!(jitter < glow, "jitter applied before the glow");
        assert_eq!(ext.jitter_amplitude, CELL_JITTER_AMPLITUDE);
        assert_eq!(ext.jitter_cell_size_m, CELL_SIZE_M);
        assert_eq!(ext.jitter_inset_m, CELL_JITTER_INSET_M);
        assert!(
            ext.jitter_inset_m > 0.0 && ext.jitter_inset_m < CELL_SIZE_M / 100.0,
            "a hair inside: {}",
            ext.jitter_inset_m
        );
    }

    #[test]
    fn jitter_does_not_change_mesh_merging() {
        // The jitter is on, and lives in the fragment shader only.
        assert!(EmissiveFromAlpha::default().jitter_amplitude > 0.0);

        let table =
            Arc::new(MaterialTable::load(MATERIALS_PATH).expect("assets/materials.ron loads"));
        let pos = ChunkPos::new(0, 0, 0);
        let n = CHUNK_SIZE;
        let id = |key: &str| table.id(key).unwrap_or_else(|| panic!("material {key:?}"));

        // A flat basalt wall across the chunk, one cell thick, and a flat
        // topsoil field: each still meshes to one quad per side.
        let mut wall = MeshInput::empty(pos, 0, table.clone());
        let mut field = MeshInput::empty(pos, 0, table.clone());
        for a in 0..n {
            for b in 0..n {
                wall.set(a, b, 10, id("basalt"));
                field.set(a, 5, b, id("topsoil"));
            }
        }
        for (name, input, face) in [("wall", &wall, IVec3::NEG_Z), ("field", &field, IVec3::Y)] {
            let data = GreedyMesher.mesh(input);
            assert_eq!(data.quad_count, 6, "the {name} is one quad per side");
            let normal = face.as_vec3().to_array();
            let front: Vec<usize> = (0..data.normals.len())
                .filter(|&i| data.normals[i] == normal)
                .collect();
            assert_eq!(front.len(), 4, "the {name}'s face is one quad");
            let (min, max) = front.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), &i| {
                let p = Vec3::from(data.positions[i]);
                (lo.min(p), hi.max(p))
            });
            let span = (max - min) / CELL_SIZE_M;
            assert_eq!(
                span.max_element(),
                n as f32,
                "the {name}'s face spans the chunk: {span}"
            );
            // The mesh carries no per-cell variation: the face's corners
            // share one color.
            let c0 = data.colors[front[0]];
            for &i in &front {
                assert_eq!(data.colors[i], c0, "one color on the {name}'s face");
            }
        }
    }

    #[test]
    fn emissive_extension_compiles() {
        // The extended material is a Material Bevy can draw.
        is_material::<ChunkEmissiveMaterial>();

        // Only the fragment stage is replaced; the base material's vertex,
        // prepass and shadow shaders and its (opaque) alpha mode are used.
        match EmissiveFromAlpha::fragment_shader() {
            ShaderRef::Path(path) => assert_eq!(path, AssetPath::from(EMISSIVE_SHADER_PATH)),
            ShaderRef::Default => {
                panic!("fragment shader is the embedded emissive.wesl, got Default")
            }
            ShaderRef::Handle(_) => {
                panic!("fragment shader is the embedded emissive.wesl, got a handle")
            }
        }
        assert!(matches!(
            EmissiveFromAlpha::vertex_shader(),
            ShaderRef::Default
        ));
        assert!(matches!(
            EmissiveFromAlpha::prepass_fragment_shader(),
            ShaderRef::Default
        ));
        assert!(matches!(
            EmissiveFromAlpha::deferred_fragment_shader(),
            ShaderRef::Default
        ));
        assert_eq!(EmissiveFromAlpha::alpha_mode(), None);
        assert!(EmissiveFromAlpha::enable_shadows());
        assert_eq!(EmissiveFromAlpha::default().emissive_scale, EMISSIVE_SCALE);

        // The shader is well-formed WESL: imports only from bevy_pbr, one
        // @fragment entry point named `fragment`, and the scale uniform at
        // the binding the derive declares, in the material bind group.
        let unit: TranslationUnit = EMISSIVE_SHADER
            .parse()
            .unwrap_or_else(|e| panic!("emissive.wesl parses: {e}"));
        assert!(!unit.imports.is_empty());
        for import in &unit.imports {
            let text = import.to_string();
            assert!(text.contains("bevy_pbr"), "import from bevy_pbr: {text}");
        }
        let mut fragments = Vec::new();
        let mut scale_bindings = Vec::new();
        for decl in &unit.global_declarations {
            match &**decl {
                GlobalDeclaration::Function(f) => {
                    if f.attributes
                        .iter()
                        .any(|a| matches!(**a, Attribute::Fragment))
                    {
                        fragments.push(f.ident.name().clone());
                    }
                }
                GlobalDeclaration::Declaration(d) if *d.ident.name() == "emissive_scale" => {
                    let mut binding = None;
                    let mut group = None;
                    for a in &d.attributes {
                        match &**a {
                            Attribute::Binding(e) => binding = Some(e.to_string()),
                            Attribute::Group(e) => group = Some(e.to_string()),
                            _ => {}
                        }
                    }
                    scale_bindings.push((binding, group, d.ty.as_ref().map(|t| t.to_string())));
                }
                _ => {}
            }
        }
        assert_eq!(fragments, ["fragment"], "one fragment entry point");
        assert_eq!(
            scale_bindings,
            [(
                Some("100".to_string()),
                Some("constants::MATERIAL_BIND_GROUP".to_string()),
                Some("f32".to_string())
            )],
            "emissive_scale matches #[uniform(100)] emissive_scale: f32"
        );
        // The strength is read before alpha_discard makes the opaque alpha 1.
        let read = EMISSIVE_SHADER
            .find("base_color.a")
            .expect("reads the color alpha");
        let discard = EMISSIVE_SHADER
            .find("= alpha_discard(")
            .expect("calls alpha_discard");
        assert!(read < discard, "alpha read before alpha_discard");

        // With the plugin, chunk meshes get the one shared emissive material:
        // the chunk StandardMaterial (white, opaque) plus the extension.
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .insert_resource(Assets::<StandardMaterial>::default())
            .add_plugins((EmissiveMaterialPlugin, GameRenderPlugin));
        app.update();
        let spawn = |app: &mut App, x: i32| {
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
        };
        let a = spawn(&mut app, 0);
        let b = spawn(&mut app, 1);
        app.update();
        let c = spawn(&mut app, 2);
        app.update();

        let handle = app
            .world()
            .get_resource::<EmissiveChunkMaterial>()
            .expect("EmissiveChunkMaterial created at startup")
            .0
            .clone();
        for e in [a, b, c] {
            let m = app
                .world()
                .get::<MeshMaterial3d<ChunkEmissiveMaterial>>(e)
                .expect("chunk mesh has the emissive material");
            assert_eq!(m.0, handle, "one shared material");
            assert!(
                app.world()
                    .get::<MeshMaterial3d<StandardMaterial>>(e)
                    .is_none(),
                "not also the plain material"
            );
        }
        let materials = app.world().resource::<Assets<ChunkEmissiveMaterial>>();
        assert_eq!(materials.len(), 1);
        let mat = materials.get(&handle).expect("material asset");
        let base = chunk_material();
        assert_eq!(mat.base.base_color, Color::WHITE);
        assert_eq!(mat.base.alpha_mode, AlphaMode::Opaque);
        assert_eq!(mat.base.perceptual_roughness, base.perceptual_roughness);
        assert_eq!(mat.base.reflectance, base.reflectance);
        assert_eq!(
            mat.base.emissive,
            LinearRgba::BLACK,
            "glow comes from alpha"
        );
        assert!(!mat.base.unlit);
        assert_eq!(mat.extension, EmissiveFromAlpha::default());
    }
}
