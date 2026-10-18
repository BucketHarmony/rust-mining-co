//! Emissive chunk material (SAD §11.2).
//!
//! The mesher stores each vertex's emissive strength in its color alpha.
//! [`ChunkEmissiveMaterial`] is the chunk [`StandardMaterial`] extended with
//! [`EmissiveFromAlpha`], whose fragment shader (`emissive.wesl`, embedded
//! as [`EMISSIVE_SHADER_PATH`]) adds `color.rgb × color.a × emissive_scale`
//! to the emissive term, so glowing cells glow in their own color and feed
//! the camera's bloom. Everything else is shaded as by the StandardMaterial.
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

use crate::render::chunk_material;

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

/// Material extension: emission from the vertex color's alpha.
#[derive(Asset, AsBindGroup, TypePath, Clone, Debug, PartialEq)]
pub struct EmissiveFromAlpha {
    /// Multiplies `color.rgb × color.a` before it is added to emission.
    #[uniform(100)]
    pub emissive_scale: f32,
}

impl Default for EmissiveFromAlpha {
    fn default() -> Self {
        Self {
            emissive_scale: EMISSIVE_SCALE,
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

/// Registers [`ChunkEmissiveMaterial`] and creates [`EmissiveChunkMaterial`].
pub struct EmissiveMaterialPlugin;

impl Plugin for EmissiveMaterialPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "emissive.wesl");
        app.add_plugins(MaterialPlugin::<ChunkEmissiveMaterial>::default())
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
    use crate::meshpipe::ChunkMesh;
    use crate::render::GameRenderPlugin;
    use bevy::asset::{AssetPath, AssetPlugin};
    use bevy::pbr::Material;
    use rmc_world::coords::ChunkPos;
    use wesl::syntax::{Attribute, GlobalDeclaration, TranslationUnit};

    fn is_material<M: Material>() {}

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
