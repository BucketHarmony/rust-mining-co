//! Tile textures for chunk faces.
//!
//! `assets/tiles.ron` maps material keys to PNG files in `assets/tiles/`, one
//! per face direction (top, side, bottom). It is read by the client only:
//! the material table, its hash, saves and the host never see it, so the art
//! can change without touching the simulation.
//!
//! [`TileSet::load`] decodes every listed file, resizes it to the configured
//! square size and builds its mip chain; [`TileSet::image`] stacks them into
//! one 2D array texture (sRGB, linear filtering, repeat addressing).
//! [`TileLookup`] tells the chunk shader, per material id, which layer each
//! face direction uses (layer + 1, 0 for none) and the material's linear
//! color, from which the shader recovers the face shade × AO the mesher baked
//! into the vertex color. Faces of materials without a tile keep their flat
//! color. A listed file that is missing or does not decode is skipped with a
//! warning, so art can be added one tile at a time.
//!
//! [`TilesPlugin`] loads the set from the [`TileDir`] resource at startup and
//! again when the editor's Reload-tiles action ([`EditorState::tiles_reload_requested`],
//! F7) asks, and hands it to the shared [`EmissiveChunkMaterial`]. Without a
//! [`TileDir`] or a `tiles.ron` nothing changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, ShaderType, TextureDataOrder, TextureDimension, TextureFormat, TextureViewDescriptor,
    TextureViewDimension,
};
use image::imageops::FilterType;
use rmc_world::materials::MaterialTable;
use serde::Deserialize;
use tracing::{info, warn};

use crate::editor::EditorState;
use crate::material::{ChunkEmissiveMaterial, EmissiveChunkMaterial};
use crate::net::ClientMaterials;

/// The tile table, in the tile directory's parent (`assets/`).
pub const TILES_FILE: &str = "tiles.ron";

/// The directory, next to [`TILES_FILE`], that its file names are relative to.
pub const TILES_SUBDIR: &str = "tiles";

/// Material ids the shader's lookup covers. Materials past it are drawn with
/// their flat color.
pub const MAX_TILE_MATERIALS: usize = 256;

/// Anisotropic filtering for tiles seen at a grazing angle.
pub const TILE_ANISOTROPY: u16 = 8;

/// Face directions, in [`TileLookup::faces`] component order.
const TOP: usize = 0;
const SIDE: usize = 1;
const BOTTOM: usize = 2;

/// `assets/tiles.ron`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TileConfig {
    /// Edge length in pixels every tile is resized to; a power of two.
    pub size: u32,
    /// Material key to its faces' files.
    pub materials: BTreeMap<String, FaceFiles>,
}

impl TileConfig {
    /// Parses `tiles.ron`. File names are plain strings: `Option` fields
    /// take a bare value (`all: "stone.png"`, no `Some(...)`).
    pub fn parse(text: &str) -> Result<Self, ron::error::SpannedError> {
        ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
            .from_str(text)
    }
}

/// One material's tile files, relative to `assets/tiles/`. `all` is the
/// default for every face; `bottom` falls back to `all`, then `side`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct FaceFiles {
    pub all: Option<String>,
    pub top: Option<String>,
    pub side: Option<String>,
    pub bottom: Option<String>,
}

impl FaceFiles {
    /// The file for each face direction: top, side, bottom.
    pub fn resolved(&self) -> [Option<&str>; 3] {
        let all = self.all.as_deref();
        let side = self.side.as_deref().or(all);
        [
            self.top.as_deref().or(all),
            side,
            self.bottom.as_deref().or(all).or(side),
        ]
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TileError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        source: Box<ron::error::SpannedError>,
    },
    #[error("tile size {0} is not a power of two")]
    Size(u32),
}

/// Every tile decoded, resized and mipmapped, ready to stack.
#[derive(Clone, Debug, PartialEq)]
pub struct TileSet {
    pub size: u32,
    pub mip_levels: u32,
    /// Layer `i`'s file name.
    pub layers: Vec<String>,
    /// RGBA8 sRGB texels, layer-major: each layer's mips from the largest.
    pub data: Vec<u8>,
    /// Material key to the layer of each face direction (top, side, bottom).
    pub faces: BTreeMap<String, [Option<u32>; 3]>,
}

impl TileSet {
    /// Loads `dir/tiles.ron` and the files it lists from `dir/tiles/`.
    /// `Ok(None)` when there is no `tiles.ron`.
    pub fn load(dir: &Path) -> Result<Option<Self>, TileError> {
        let path = dir.join(TILES_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(TileError::Read { path, source }),
        };
        let config = TileConfig::parse(&text).map_err(|e| TileError::Parse {
            path,
            source: Box::new(e),
        })?;
        Self::build(&config, &dir.join(TILES_SUBDIR)).map(Some)
    }

    /// The set `config` describes, with its files read from `tile_dir`.
    pub fn build(config: &TileConfig, tile_dir: &Path) -> Result<Self, TileError> {
        let size = config.size;
        if !size.is_power_of_two() {
            return Err(TileError::Size(size));
        }
        let mut set = TileSet {
            size,
            mip_levels: size.trailing_zeros() + 1,
            layers: Vec::new(),
            data: Vec::new(),
            faces: BTreeMap::new(),
        };
        let mut layer_of: BTreeMap<String, Option<u32>> = BTreeMap::new();
        for (key, files) in &config.materials {
            let faces = files.resolved().map(|file| {
                let file = file?;
                *layer_of
                    .entry(file.to_owned())
                    .or_insert_with(|| set.push_layer(&tile_dir.join(file), file))
            });
            set.faces.insert(key.clone(), faces);
        }
        Ok(set)
    }

    /// Decodes `path` and appends it as a layer, or warns and returns `None`.
    fn push_layer(&mut self, path: &Path, name: &str) -> Option<u32> {
        let img = match image::open(path) {
            Ok(img) => img.to_rgba8(),
            Err(e) => {
                warn!("tile {}: {e}", path.display());
                return None;
            }
        };
        let mut level = self.size;
        while level > 0 {
            let mip = image::imageops::resize(&img, level, level, FilterType::Lanczos3);
            self.data.extend_from_slice(mip.as_raw());
            level /= 2;
        }
        self.layers.push(name.to_owned());
        Some(self.layers.len() as u32 - 1)
    }

    /// The 2D array texture of every layer, with its mips.
    pub fn image(&self) -> Image {
        // The texture needs at least one layer; an empty set gets one
        // transparent layer that no face points at.
        let (layers, data) = if self.layers.is_empty() {
            (1, vec![0; self.layer_bytes()])
        } else {
            (self.layers.len() as u32, self.data.clone())
        };
        let mut image = Image::new_uninit(
            Extent3d {
                width: self.size,
                height: self.size,
                depth_or_array_layers: layers,
            },
            TextureDimension::D2,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.data = Some(data);
        image.data_order = TextureDataOrder::LayerMajor;
        image.texture_descriptor.mip_level_count = self.mip_levels;
        image.texture_view_descriptor = Some(TextureViewDescriptor {
            dimension: Some(TextureViewDimension::D2Array),
            ..default()
        });
        image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
            address_mode_u: ImageAddressMode::Repeat,
            address_mode_v: ImageAddressMode::Repeat,
            mag_filter: ImageFilterMode::Linear,
            min_filter: ImageFilterMode::Linear,
            mipmap_filter: ImageFilterMode::Linear,
            anisotropy_clamp: TILE_ANISOTROPY,
            ..default()
        });
        image
    }

    /// Bytes of one layer with all its mips.
    fn layer_bytes(&self) -> usize {
        (0..self.mip_levels)
            .map(|m| ((self.size >> m) as usize).pow(2) * 4)
            .sum()
    }

    /// The shader's lookup for `materials`: matched by key.
    pub fn lookup(&self, materials: &MaterialTable) -> TileLookup {
        let mut lookup = TileLookup::default();
        for def in materials.iter() {
            let i = usize::from(def.id.0);
            if i >= MAX_TILE_MATERIALS {
                warn!("material {} (id {i}) is past the tile lookup", def.key);
                continue;
            }
            let [r, g, b, _] = def.color;
            let c = Color::srgb_u8(r, g, b).to_linear();
            lookup.base[i] = Vec4::new(c.red, c.green, c.blue, 1.0);
            if let Some(faces) = self.faces.get(&def.key) {
                let slot = |f: usize| faces[f].map_or(0, |layer| layer + 1);
                lookup.faces[i] = UVec4::new(slot(TOP), slot(SIDE), slot(BOTTOM), 0);
            }
        }
        lookup
    }
}

/// Per material id: the tile layer + 1 of its top, side and bottom faces
/// (0: flat color), and its linear color.
#[derive(Clone, Debug, PartialEq, ShaderType)]
pub struct TileLookup {
    pub faces: [UVec4; MAX_TILE_MATERIALS],
    pub base: [Vec4; MAX_TILE_MATERIALS],
}

impl Default for TileLookup {
    fn default() -> Self {
        Self {
            faces: [UVec4::ZERO; MAX_TILE_MATERIALS],
            base: [Vec4::ONE; MAX_TILE_MATERIALS],
        }
    }
}

/// The directory holding `tiles.ron` (the game's `assets/`).
#[derive(Resource, Clone, Debug)]
pub struct TileDir(pub PathBuf);

/// The tile set the chunk material was last given.
#[derive(Resource, Clone, Debug)]
pub struct LoadedTiles(pub TileSet);

/// Loads tiles from [`TileDir`] at startup and on the editor's Reload-tiles
/// action, and applies them to the chunk material.
pub struct TilesPlugin;

impl Plugin for TilesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostStartup, load_tiles)
            .add_systems(Update, reload_tiles);
    }
}

fn load_tiles(world: &mut World) {
    let Some(dir) = world.get_resource::<TileDir>().cloned() else {
        return;
    };
    apply(world, &dir.0);
}

fn reload_tiles(world: &mut World) {
    let requested = world
        .get_resource_mut::<EditorState>()
        .is_some_and(|mut s| std::mem::take(&mut s.tiles_reload_requested));
    if requested && let Some(dir) = world.get_resource::<TileDir>().cloned() {
        apply(world, &dir.0);
    }
}

/// Loads the set in `dir` and gives it to the shared chunk material.
fn apply(world: &mut World, dir: &Path) {
    let set = match TileSet::load(dir) {
        Ok(Some(set)) => set,
        Ok(None) => return,
        Err(e) => {
            warn!("tiles not loaded: {e}");
            return;
        }
    };
    info!(
        "tiles: {} layers at {}px for {} materials",
        set.layers.len(),
        set.size,
        set.faces.len()
    );
    let (Some(materials), Some(handle)) = (
        world.get_resource::<ClientMaterials>().map(|m| m.0.clone()),
        world
            .get_resource::<EmissiveChunkMaterial>()
            .map(|m| m.0.clone()),
    ) else {
        world.insert_resource(LoadedTiles(set));
        return;
    };
    let lookup = set.lookup(&materials);
    let image = world
        .get_resource_mut::<Assets<Image>>()
        .map(|mut images| images.add(set.image()));
    if let Some(mut chunk_materials) = world.get_resource_mut::<Assets<ChunkEmissiveMaterial>>()
        && let Some(mut material) = chunk_materials.get_mut(&handle)
    {
        material.extension.tiles = image;
        material.extension.tile_lookup = lookup;
    }
    world.insert_resource(LoadedTiles(set));
}

#[cfg(test)]
mod tests {
    use super::*;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");
    const ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rmc_tiles_{name}_{}", std::process::id()));
        std::fs::create_dir_all(dir.join(TILES_SUBDIR)).unwrap();
        dir
    }

    fn write_png(path: &Path, size: u32, rgb: [u8; 3]) {
        image::RgbaImage::from_pixel(size, size, image::Rgba([rgb[0], rgb[1], rgb[2], 255]))
            .save(path)
            .unwrap();
    }

    #[test]
    fn face_files_fall_back_to_all_then_side() {
        let only_all = FaceFiles {
            all: Some("a.png".into()),
            ..default()
        };
        assert_eq!(only_all.resolved(), [Some("a.png"); 3]);
        let grass = FaceFiles {
            top: Some("grass.png".into()),
            side: Some("grass_side.png".into()),
            ..default()
        };
        assert_eq!(
            grass.resolved(),
            [
                Some("grass.png"),
                Some("grass_side.png"),
                Some("grass_side.png")
            ]
        );
    }

    #[test]
    fn build_resizes_mips_and_shares_layers() {
        let dir = scratch_dir("build");
        write_png(&dir.join("tiles/a.png"), 64, [200, 10, 10]);
        write_png(&dir.join("tiles/b.png"), 16, [10, 200, 10]);
        let config = TileConfig::parse(
            r#"(size: 32, materials: {
                "stone": (all: "a.png"),
                "topsoil": (top: "b.png", side: "a.png"),
                "dirt": (all: "missing.png"),
            })"#,
        )
        .unwrap();
        let set = TileSet::build(&config, &dir.join(TILES_SUBDIR)).unwrap();
        assert_eq!(set.size, 32);
        assert_eq!(set.mip_levels, 6, "32, 16, 8, 4, 2, 1");
        assert_eq!(set.layers.len(), 2, "a.png is one layer for both materials");
        assert_eq!(set.data.len(), 2 * set.layer_bytes());
        assert_eq!(set.faces["stone"], [Some(0); 3]);
        assert_eq!(set.faces["topsoil"], [Some(1), Some(0), Some(0)]);
        assert_eq!(set.faces["dirt"], [None; 3], "a missing file is skipped");
        // Every mip of a flat tile keeps its color.
        assert!(
            set.data
                .chunks(4)
                .take(set.layer_bytes() / 4)
                .all(|p| p == [200, 10, 10, 255])
        );

        let image = set.image();
        assert_eq!(image.texture_descriptor.size.depth_or_array_layers, 2);
        assert_eq!(image.texture_descriptor.mip_level_count, 6);
        assert_eq!(
            image.texture_descriptor.format,
            TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            image
                .texture_view_descriptor
                .as_ref()
                .and_then(|v| v.dimension),
            Some(TextureViewDimension::D2Array)
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn size_must_be_a_power_of_two() {
        let config = TileConfig {
            size: 48,
            materials: BTreeMap::new(),
        };
        assert!(matches!(
            TileSet::build(&config, Path::new(".")),
            Err(TileError::Size(48))
        ));
    }

    #[test]
    fn lookup_matches_materials_by_key() {
        let table = MaterialTable::load(MATERIALS_PATH).unwrap();
        let set = TileSet {
            size: 1,
            mip_levels: 1,
            layers: vec!["a".into(), "b".into()],
            data: vec![0; 8],
            faces: BTreeMap::from([("topsoil".to_owned(), [Some(1), Some(0), None])]),
        };
        let lookup = set.lookup(&table);
        let topsoil = usize::from(table.id("topsoil").unwrap().0);
        let stone = usize::from(table.id("stone").unwrap().0);
        assert_eq!(lookup.faces[topsoil], UVec4::new(2, 1, 0, 0));
        assert_eq!(lookup.faces[stone], UVec4::ZERO, "no tile: flat color");
        let [r, g, b, _] = table.get(table.id("stone").unwrap()).unwrap().color;
        let c = Color::srgb_u8(r, g, b).to_linear();
        assert_eq!(lookup.base[stone], Vec4::new(c.red, c.green, c.blue, 1.0));
    }

    #[test]
    fn shipped_tiles_load_for_shipped_materials() {
        let set = TileSet::load(Path::new(ASSETS_DIR))
            .expect("assets/tiles.ron parses")
            .expect("assets/tiles.ron exists");
        let table = MaterialTable::load(MATERIALS_PATH).unwrap();
        for key in set.faces.keys() {
            assert!(
                table.id(key).is_some(),
                "tiles.ron names unknown material {key:?}"
            );
        }
        assert!(!set.layers.is_empty(), "at least one tile decodes");
    }

    #[test]
    fn missing_table_is_no_tiles() {
        let dir = scratch_dir("none");
        assert_eq!(TileSet::load(&dir).unwrap(), None);
        std::fs::remove_dir_all(dir).ok();
    }
}
