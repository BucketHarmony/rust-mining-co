//! Generation parameters loaded from `assets/worldgen.ron` (SAD §9).
//!
//! [`GenParams::hash`] goes into `Welcome` and the save header. Materials are
//! named by key here and resolved against the material table by
//! [`crate::Generator::new`], which also checks their tags.

use std::path::Path;

use glam::IVec3;
use rmc_world::coords::{CellAabb, CellPos};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Largest amplitude, thickness or depth a parameter may hold, in cells.
/// Keeps every sum the generator forms well inside `i32`.
pub const MAX_CELLS: i32 = 1 << 20;

/// Largest `|base_height|`, in cells.
pub const MAX_BASE_HEIGHT: i32 = 1 << 28;

/// Most fractal octaves a noise layer may use.
pub const MAX_OCTAVES: u8 = 8;

/// Denominator of [`CrystalParams::chance_per_million`].
pub const ONE_MILLION: u32 = 1_000_000;

/// One fractal (FBm) noise layer. Its seed comes from the world seed and the
/// layer id, not from here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoiseParams {
    /// Per meter: noise is sampled at world meters.
    pub frequency: f32,
    pub octaves: u8,
}

/// Step 1: the 2D heightmap.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SurfaceParams {
    /// Mean y of the topmost solid cell.
    pub base_height: i32,
    /// The surface lies within `base_height ± amplitude`.
    pub amplitude: i32,
    pub noise: NoiseParams,
}

/// A soil layer under the surface. Each column draws its thickness in
/// `min_cells..=max_cells` from the layer's noise.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SoilLayer {
    /// Material key; it must carry the `soil` tag.
    pub material: String,
    pub min_cells: i32,
    pub max_cells: i32,
    pub noise: NoiseParams,
}

/// One depth band of base rock.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stratum {
    /// Material key; it must carry the `stone` tag.
    pub material: String,
    /// Depth under the surface (0 = topmost solid cell) where the next
    /// stratum starts, before noise. `None` for the last stratum only.
    pub until_depth: Option<i32>,
}

/// Step 2: strata by depth band with noise-perturbed boundaries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrataParams {
    /// Each boundary moves by up to this many cells.
    pub boundary_amplitude: i32,
    /// Shared settings; every boundary has its own seed.
    pub boundary_noise: NoiseParams,
    /// Top down.
    pub layers: Vec<Stratum>,
}

/// Blob caves: air wherever the 3D noise exceeds `threshold`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlobCaves {
    /// Noise is in about [-1, 1]; a value above 1 turns blob caves off.
    pub threshold: f32,
    pub noise: NoiseParams,
}

/// Worm tunnels: air wherever both 3D noises lie within `±radius` of zero.
/// Each noise is near zero on a thin sheet, and two sheets meet in a curve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WormCaves {
    /// 0 turns worm tunnels off.
    pub radius: f32,
    pub a: NoiseParams,
    pub b: NoiseParams,
}

/// Step 3: caves carved to air under the surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaveParams {
    /// Caves never carve the surface cell or the `surface_margin` cells
    /// under it (depths `0..=surface_margin`).
    pub surface_margin: i32,
    pub blob: BlobCaves,
    pub worm: WormCaves,
}

/// Step 4: one ore. It replaces cells tagged `stone` where its 3D noise
/// exceeds `threshold`, within its depth range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OreVein {
    /// Material key; it must carry the `ore` tag.
    pub material: String,
    /// Depth range under the surface cell (0 = topmost solid cell, as for
    /// strata), both ends included.
    pub min_depth: i32,
    pub max_depth: i32,
    /// Noise is in about [-1, 1]; a value above 1 turns the ore off.
    pub threshold: f32,
    pub noise: NoiseParams,
}

/// Step 5: crystals on cells tagged `stone` that are face-adjacent to air.
/// Each such cell is chosen by a hash of its position and the seed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CrystalParams {
    /// Material key; it must carry the `crystal` tag.
    pub material: String,
    /// Share of candidate cells that become crystal, in millionths
    /// (`0..=1_000_000`; 0 turns crystals off).
    pub chance_per_million: u32,
}

/// The water table (SAD-P2 §8): porous cells below it hold their
/// `field_capacity` of moisture and caves below it are flooded. Each column
/// has its own table at `water_table_y = level + offset`, the offset in
/// `-amplitude..=amplitude` from the table's noise. With amplitude 0 the
/// table is level, so the water worldgen places is at rest; a tilted table
/// spills into the caves beside it once the water sweep visits it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WaterTableParams {
    /// Mean y of the table: cells with a lower y are below it.
    pub level: i32,
    /// The table moves by up to this many cells.
    pub amplitude: i32,
    pub noise: NoiseParams,
}

/// Magma chambers (SAD-P2 §8): `count_min..=count_max` blobs, each a ball
/// of `radius_min..=radius_max` cells around a hash-picked center whose
/// boundary moves by up to `amplitude` cells of 3D noise, with a core of the
/// cells within `core_radius` of the center. Every chamber cell lies below
/// `chamber_max_y` and more than `shell_clearance` cells from the bedrock
/// shell on each axis. Materials come from the eco roles `magma` and
/// `magma_source`, so a table without them has no chambers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MagmaChamberParams {
    pub count_min: u8,
    pub count_max: u8,
    /// Every chamber cell has a lower y.
    pub chamber_max_y: i32,
    pub radius_min: i32,
    pub radius_max: i32,
    /// Below `radius_min`, so every chamber holds its center.
    pub amplitude: i32,
    pub noise: NoiseParams,
    /// At most `radius_min - amplitude`, so the core lies inside the blob.
    pub core_radius: i32,
    /// Cells between a chamber and the shell, on each axis.
    pub shell_clearance: i32,
}

/// The world's extent in cells, both corners included (SAD §5). Step 6
/// builds its shell inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldBounds {
    pub min: [i32; 3],
    pub max: [i32; 3],
}

impl WorldBounds {
    /// The same box as the world's bounds type.
    pub fn aabb(&self) -> CellAabb {
        CellAabb::new(
            CellPos(IVec3::from_array(self.min)),
            CellPos(IVec3::from_array(self.max)),
        )
    }
}

/// Step 6: the bedrock shell inside the world bounds, over everything else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BedrockParams {
    /// Material key; it must be solid and not diggable.
    pub material: String,
    /// Layers at the bottom of the world.
    pub floor_cells: i32,
    /// Columns at each x and z edge of the world, full height.
    pub wall_cells: i32,
}

/// Everything worldgen reads besides the seed and the material table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenParams {
    pub bounds: WorldBounds,
    pub surface: SurfaceParams,
    /// Top down from the surface.
    pub soil: Vec<SoilLayer>,
    pub strata: StrataParams,
    pub caves: CaveParams,
    /// Step 4. Where two ores overlap, the earlier one wins.
    pub ores: Vec<OreVein>,
    pub crystals: CrystalParams,
    pub bedrock: BedrockParams,
    /// SAD-P2 §8; `None` (the default) leaves the world dry.
    #[serde(default)]
    pub water_table: Option<WaterTableParams>,
    /// SAD-P2 §8; `None` (the default) places no magma.
    #[serde(default)]
    pub magma_chambers: Option<MagmaChamberParams>,
}

#[derive(Debug, Error)]
pub enum GenError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing worldgen RON: {0}")]
    Parse(#[from] ron::error::SpannedError),
    #[error("{field}: {reason}")]
    Invalid { field: String, reason: String },
    #[error("{field}: material {key:?} is not in the material table")]
    UnknownMaterial { field: String, key: String },
    #[error("{field}: material {key:?} lacks the {tag:?} tag")]
    MissingTag {
        field: String,
        key: String,
        tag: &'static str,
    },
    #[error("{field}: material {key:?} must be solid and not diggable")]
    Diggable { field: String, key: String },
}

fn invalid(field: impl Into<String>, reason: impl Into<String>) -> GenError {
    GenError::Invalid {
        field: field.into(),
        reason: reason.into(),
    }
}

impl NoiseParams {
    fn validate(&self, field: &str) -> Result<(), GenError> {
        if !self.frequency.is_finite() || self.frequency <= 0.0 {
            return Err(invalid(
                field,
                format!("frequency {} must be finite and > 0", self.frequency),
            ));
        }
        if !(1..=MAX_OCTAVES).contains(&self.octaves) {
            return Err(invalid(
                field,
                format!("octaves {} must be in 1..={MAX_OCTAVES}", self.octaves),
            ));
        }
        Ok(())
    }
}

fn check_cells(field: &str, value: i32) -> Result<(), GenError> {
    if !(0..=MAX_CELLS).contains(&value) {
        return Err(invalid(
            field,
            format!("{value} must be in 0..={MAX_CELLS}"),
        ));
    }
    Ok(())
}

impl GenParams {
    /// Loads and validates parameters from a RON file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, GenError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| GenError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let params = Self::from_ron_str(&text)?;
        tracing::info!(
            path = %path.display(),
            hash = format_args!("{:016x}", params.hash()),
            "worldgen params loaded"
        );
        Ok(params)
    }

    /// Parses and validates parameters from RON text.
    pub fn from_ron_str(text: &str) -> Result<Self, GenError> {
        let params: Self = ron::from_str(text)?;
        params.validate()?;
        Ok(params)
    }

    /// Checks ranges and ordering. Material keys are checked against the
    /// table later, by [`crate::Generator::new`].
    pub fn validate(&self) -> Result<(), GenError> {
        let b = &self.bounds;
        for axis in 0..3 {
            for (name, value) in [("min", b.min[axis]), ("max", b.max[axis])] {
                if !(-MAX_BASE_HEIGHT..=MAX_BASE_HEIGHT).contains(&value) {
                    return Err(invalid(
                        format!("bounds.{name}[{axis}]"),
                        format!("{value} must be within ±{MAX_BASE_HEIGHT}"),
                    ));
                }
            }
            if b.min[axis] > b.max[axis] {
                return Err(invalid(
                    format!("bounds.min[{axis}]"),
                    format!("{} exceeds max {}", b.min[axis], b.max[axis]),
                ));
            }
        }

        let s = &self.surface;
        if !(-MAX_BASE_HEIGHT..=MAX_BASE_HEIGHT).contains(&s.base_height) {
            return Err(invalid(
                "surface.base_height",
                format!("{} must be within ±{MAX_BASE_HEIGHT}", s.base_height),
            ));
        }
        check_cells("surface.amplitude", s.amplitude)?;
        s.noise.validate("surface.noise")?;

        for (i, layer) in self.soil.iter().enumerate() {
            let field = format!("soil[{i}]");
            check_cells(&format!("{field}.min_cells"), layer.min_cells)?;
            check_cells(&format!("{field}.max_cells"), layer.max_cells)?;
            if layer.min_cells > layer.max_cells {
                return Err(invalid(
                    field,
                    format!(
                        "min_cells {} exceeds max_cells {}",
                        layer.min_cells, layer.max_cells
                    ),
                ));
            }
            layer.noise.validate(&format!("{field}.noise"))?;
        }

        let st = &self.strata;
        check_cells("strata.boundary_amplitude", st.boundary_amplitude)?;
        st.boundary_noise.validate("strata.boundary_noise")?;
        let Some((last, upper)) = st.layers.split_last() else {
            return Err(invalid("strata.layers", "needs at least one stratum"));
        };
        if last.until_depth.is_some() {
            return Err(invalid(
                format!("strata.layers[{}]", upper.len()),
                "the last stratum must have until_depth: None",
            ));
        }
        let mut prev = 0;
        for (i, layer) in upper.iter().enumerate() {
            let field = format!("strata.layers[{i}].until_depth");
            let Some(depth) = layer.until_depth else {
                return Err(invalid(field, "only the last stratum may be None"));
            };
            check_cells(&field, depth)?;
            if depth <= prev {
                return Err(invalid(
                    field,
                    format!("{depth} must exceed the previous boundary {prev}"),
                ));
            }
            prev = depth;
        }

        let c = &self.caves;
        check_cells("caves.surface_margin", c.surface_margin)?;
        if !c.blob.threshold.is_finite() {
            return Err(invalid(
                "caves.blob.threshold",
                format!("{} must be finite", c.blob.threshold),
            ));
        }
        c.blob.noise.validate("caves.blob.noise")?;
        if !c.worm.radius.is_finite() || c.worm.radius < 0.0 {
            return Err(invalid(
                "caves.worm.radius",
                format!("{} must be finite and >= 0", c.worm.radius),
            ));
        }
        c.worm.a.validate("caves.worm.a")?;
        c.worm.b.validate("caves.worm.b")?;

        for (i, ore) in self.ores.iter().enumerate() {
            let field = format!("ores[{i}]");
            check_cells(&format!("{field}.min_depth"), ore.min_depth)?;
            check_cells(&format!("{field}.max_depth"), ore.max_depth)?;
            if ore.min_depth > ore.max_depth {
                return Err(invalid(
                    field,
                    format!(
                        "min_depth {} exceeds max_depth {}",
                        ore.min_depth, ore.max_depth
                    ),
                ));
            }
            if !ore.threshold.is_finite() {
                return Err(invalid(
                    format!("{field}.threshold"),
                    format!("{} must be finite", ore.threshold),
                ));
            }
            ore.noise.validate(&format!("{field}.noise"))?;
        }

        let chance = self.crystals.chance_per_million;
        if chance > ONE_MILLION {
            return Err(invalid(
                "crystals.chance_per_million",
                format!("{chance} must be in 0..={ONE_MILLION}"),
            ));
        }

        check_cells("bedrock.floor_cells", self.bedrock.floor_cells)?;
        check_cells("bedrock.wall_cells", self.bedrock.wall_cells)?;

        if let Some(w) = &self.water_table {
            if !(-MAX_BASE_HEIGHT..=MAX_BASE_HEIGHT).contains(&w.level) {
                return Err(invalid(
                    "water_table.level",
                    format!("{} must be within ±{MAX_BASE_HEIGHT}", w.level),
                ));
            }
            check_cells("water_table.amplitude", w.amplitude)?;
            w.noise.validate("water_table.noise")?;
        }

        if let Some(m) = &self.magma_chambers {
            if m.count_min > m.count_max {
                return Err(invalid(
                    "magma_chambers.count_min",
                    format!("{} exceeds count_max {}", m.count_min, m.count_max),
                ));
            }
            if !(-MAX_BASE_HEIGHT..=MAX_BASE_HEIGHT).contains(&m.chamber_max_y) {
                return Err(invalid(
                    "magma_chambers.chamber_max_y",
                    format!("{} must be within ±{MAX_BASE_HEIGHT}", m.chamber_max_y),
                ));
            }
            check_cells("magma_chambers.radius_min", m.radius_min)?;
            check_cells("magma_chambers.radius_max", m.radius_max)?;
            check_cells("magma_chambers.amplitude", m.amplitude)?;
            check_cells("magma_chambers.core_radius", m.core_radius)?;
            check_cells("magma_chambers.shell_clearance", m.shell_clearance)?;
            if m.radius_min > m.radius_max {
                return Err(invalid(
                    "magma_chambers.radius_min",
                    format!("{} exceeds radius_max {}", m.radius_min, m.radius_max),
                ));
            }
            if m.amplitude >= m.radius_min {
                return Err(invalid(
                    "magma_chambers.amplitude",
                    format!("{} must be below radius_min {}", m.amplitude, m.radius_min),
                ));
            }
            if m.core_radius > m.radius_min - m.amplitude {
                return Err(invalid(
                    "magma_chambers.core_radius",
                    format!(
                        "{} exceeds radius_min - amplitude {}",
                        m.core_radius,
                        m.radius_min - m.amplitude
                    ),
                ));
            }
            m.noise.validate("magma_chambers.noise")?;
        }
        Ok(())
    }

    /// These parameters for a world of `bounds`: the bounds replaced and the
    /// surface's base height, the water table's level and the chambers'
    /// `chamber_max_y` moved to the same fraction of the world's height
    /// (truncating), so a lower world keeps its share of ground and sky
    /// (SAD-P2 §2: the 64-cell-high Phase 2 world). Everything else is kept.
    /// Bounds of the same height and floor give these parameters unchanged.
    /// Fails if the result does not validate or the highest surface does not
    /// lie below the top cell.
    pub fn fit_to(&self, bounds: WorldBounds) -> Result<Self, GenError> {
        let (old_floor, new_floor) = (self.bounds.min[1], bounds.min[1]);
        let old_height = i64::from(self.bounds.max[1]) - i64::from(old_floor) + 1;
        let new_height = i64::from(bounds.max[1]) - i64::from(new_floor) + 1;
        let scale = |field: &str, y: i32| {
            let above_floor = i64::from(y) - i64::from(old_floor);
            let fitted = i64::from(new_floor) + above_floor * new_height / old_height;
            i32::try_from(fitted).map_err(|_| invalid(field, format!("{fitted} overflows i32")))
        };
        let mut fitted = self.clone();
        fitted.bounds = bounds;
        fitted.surface.base_height = scale("surface.base_height", self.surface.base_height)?;
        if let Some(w) = &mut fitted.water_table {
            w.level = scale("water_table.level", w.level)?;
        }
        if let Some(m) = &mut fitted.magma_chambers {
            m.chamber_max_y = scale("magma_chambers.chamber_max_y", m.chamber_max_y)?;
        }
        fitted.validate()?;
        let highest = i64::from(fitted.surface.base_height) + i64::from(fitted.surface.amplitude);
        if highest >= i64::from(bounds.max[1]) {
            return Err(invalid(
                "surface.base_height",
                format!(
                    "the surface reaches y {highest}, not below the top cell {}",
                    bounds.max[1]
                ),
            ));
        }
        Ok(fitted)
    }

    /// xxh3 over the postcard encoding of the parameters. Independent of
    /// RON formatting; any field change moves it.
    pub fn hash(&self) -> u64 {
        let mut buf = b"rmc.genparams.v1".to_vec();
        buf.extend(postcard::to_stdvec(self).expect("GenParams always encodes"));
        xxhash_rust::xxh3::xxh3_64(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An edit applied to a copy of the shipped params.
    type Edit = Box<dyn Fn(&mut GenParams)>;

    const PHASE1_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/worldgen.ron");

    fn phase1() -> GenParams {
        GenParams::load(PHASE1_PATH).expect("assets/worldgen.ron loads")
    }

    #[test]
    fn fit_to_scales_the_surface_to_the_height() {
        let p = phase1();
        let size = |w: i32, h: i32, d: i32| WorldBounds {
            min: [0, 0, 0],
            max: [w - 1, h - 1, d - 1],
        };
        // The same height keeps everything, whatever the width.
        assert_eq!(p.fit_to(p.bounds).unwrap(), p);
        let wide = p.fit_to(size(64, 128, 64)).unwrap();
        assert_eq!(wide.surface, p.surface);
        assert_eq!(wide.bounds, size(64, 128, 64));
        // Half the height: half the base height, the same relief.
        let low = p.fit_to(size(128, 64, 128)).unwrap();
        assert_eq!(low.surface.base_height, p.surface.base_height / 2);
        assert_eq!(low.surface.amplitude, p.surface.amplitude);
        assert_eq!((&low.soil, &low.strata), (&p.soil, &p.strata));
        assert_ne!(low.hash(), p.hash());
        // Measured from the floor.
        let raised = WorldBounds {
            min: [0, 100, 0],
            max: [127, 163, 127],
        };
        assert_eq!(
            p.fit_to(raised).unwrap().surface.base_height,
            100 + p.surface.base_height / 2
        );
        // Too low for the relief: the surface would reach the top.
        assert!(p.fit_to(size(64, 16, 64)).is_err());
    }

    #[test]
    fn loads_and_validates() {
        let p = phase1();
        assert_eq!(p.soil.len(), 2);
        // The Phase 1 strata in order; Phase 2 adds bands between them
        // (SAD-P2 §8 limestone).
        let phase1_strata: Vec<&str> = p
            .strata
            .layers
            .iter()
            .map(|l| l.material.as_str())
            .filter(|m| ["stone", "granite", "basalt"].contains(m))
            .collect();
        assert_eq!(phase1_strata, ["stone", "granite", "basalt"]);
        assert_eq!(p.strata.layers.last().unwrap().until_depth, None);
        assert_eq!(p.ores.len(), 3);
        assert_eq!(p.crystals.material, "glowcrystal");
        assert_eq!(p.bounds.aabb(), CellAabb::PHASE1_DEFAULT);
        assert_eq!(p.bedrock.material, "bedrock");
        assert_eq!((p.bedrock.floor_cells, p.bedrock.wall_cells), (2, 1));

        // Each broken copy is refused with an Invalid error.
        let broken: Vec<(&str, Edit)> = vec![
            (
                "nan frequency",
                Box::new(|p| p.surface.noise.frequency = f32::NAN),
            ),
            (
                "zero frequency",
                Box::new(|p| p.surface.noise.frequency = 0.0),
            ),
            (
                "inf frequency",
                Box::new(|p| p.soil[0].noise.frequency = f32::INFINITY),
            ),
            (
                "zero octaves",
                Box::new(|p| p.strata.boundary_noise.octaves = 0),
            ),
            (
                "many octaves",
                Box::new(|p| p.surface.noise.octaves = MAX_OCTAVES + 1),
            ),
            ("negative amplitude", Box::new(|p| p.surface.amplitude = -1)),
            (
                "huge amplitude",
                Box::new(|p| p.surface.amplitude = MAX_CELLS + 1),
            ),
            ("huge base", Box::new(|p| p.surface.base_height = i32::MIN)),
            ("soil min > max", Box::new(|p| p.soil[1].min_cells = 9)),
            ("negative soil", Box::new(|p| p.soil[0].min_cells = -1)),
            ("no strata", Box::new(|p| p.strata.layers.clear())),
            (
                "bounded last",
                Box::new(|p| p.strata.layers.last_mut().unwrap().until_depth = Some(90)),
            ),
            (
                "unbounded middle",
                Box::new(|p| p.strata.layers[1].until_depth = None),
            ),
            (
                "equal boundaries",
                Box::new(|p| p.strata.layers[1].until_depth = Some(24)),
            ),
            (
                "zero boundary",
                Box::new(|p| p.strata.layers[0].until_depth = Some(0)),
            ),
            (
                "negative boundary amplitude",
                Box::new(|p| p.strata.boundary_amplitude = -2),
            ),
            (
                "negative cave margin",
                Box::new(|p| p.caves.surface_margin = -1),
            ),
            (
                "nan blob threshold",
                Box::new(|p| p.caves.blob.threshold = f32::NAN),
            ),
            (
                "inf blob threshold",
                Box::new(|p| p.caves.blob.threshold = f32::NEG_INFINITY),
            ),
            (
                "zero blob octaves",
                Box::new(|p| p.caves.blob.noise.octaves = 0),
            ),
            (
                "negative worm radius",
                Box::new(|p| p.caves.worm.radius = -0.1),
            ),
            (
                "nan worm radius",
                Box::new(|p| p.caves.worm.radius = f32::NAN),
            ),
            (
                "zero worm frequency",
                Box::new(|p| p.caves.worm.b.frequency = 0.0),
            ),
            ("negative ore depth", Box::new(|p| p.ores[0].min_depth = -1)),
            (
                "huge ore depth",
                Box::new(|p| p.ores[2].max_depth = MAX_CELLS + 1),
            ),
            (
                "ore min > max",
                Box::new(|p| p.ores[1].min_depth = p.ores[1].max_depth + 1),
            ),
            (
                "nan ore threshold",
                Box::new(|p| p.ores[0].threshold = f32::NAN),
            ),
            (
                "inf ore threshold",
                Box::new(|p| p.ores[1].threshold = f32::INFINITY),
            ),
            (
                "nine ore octaves",
                Box::new(|p| p.ores[2].noise.octaves = 9),
            ),
            (
                "crystal chance over one",
                Box::new(|p| p.crystals.chance_per_million = ONE_MILLION + 1),
            ),
            (
                "chamber count min > max",
                Box::new(|p| p.magma_chambers.as_mut().unwrap().count_min = 4),
            ),
            (
                "chamber radius min > max",
                Box::new(|p| p.magma_chambers.as_mut().unwrap().radius_min = 6),
            ),
            (
                "chamber amplitude reaches the center",
                Box::new(|p| p.magma_chambers.as_mut().unwrap().amplitude = 3),
            ),
            (
                "chamber core outside the blob",
                Box::new(|p| p.magma_chambers.as_mut().unwrap().core_radius = 3),
            ),
            (
                "negative chamber clearance",
                Box::new(|p| p.magma_chambers.as_mut().unwrap().shell_clearance = -1),
            ),
            (
                "huge chamber_max_y",
                Box::new(|p| p.magma_chambers.as_mut().unwrap().chamber_max_y = i32::MAX),
            ),
            (
                "zero chamber octaves",
                Box::new(|p| p.magma_chambers.as_mut().unwrap().noise.octaves = 0),
            ),
            ("min above max", Box::new(|p| p.bounds.min[1] = 128)),
            ("huge bounds", Box::new(|p| p.bounds.max[2] = i32::MAX)),
            (
                "huge negative bounds",
                Box::new(|p| p.bounds.min[0] = i32::MIN),
            ),
            ("negative floor", Box::new(|p| p.bedrock.floor_cells = -1)),
            (
                "huge wall",
                Box::new(|p| p.bedrock.wall_cells = MAX_CELLS + 1),
            ),
        ];
        for (name, edit) in broken {
            let mut p = phase1();
            edit(&mut p);
            assert!(
                matches!(p.validate(), Err(GenError::Invalid { .. })),
                "{name}: accepted"
            );
        }

        // Soil may be empty, and one unbounded stratum is enough.
        let mut p = phase1();
        p.soil.clear();
        p.strata.layers.truncate(1);
        p.strata.layers[0].until_depth = None;
        p.validate().unwrap();

        // Caves can be turned off: no margin, no blobs, no worms.
        let mut p = phase1();
        p.caves.surface_margin = 0;
        p.caves.blob.threshold = 2.0;
        p.caves.worm.radius = 0.0;
        p.validate().unwrap();

        // No ores, or an ore at a single depth, is fine.
        let mut p = phase1();
        p.ores[0].max_depth = p.ores[0].min_depth;
        p.validate().unwrap();
        p.ores.clear();
        p.validate().unwrap();

        // Crystals from never to every candidate cell.
        let mut p = phase1();
        for chance in [0, ONE_MILLION] {
            p.crystals.chance_per_million = chance;
            p.validate().unwrap();
        }

        // No shell, and a one-cell world.
        let mut p = phase1();
        p.bedrock.floor_cells = 0;
        p.bedrock.wall_cells = 0;
        p.validate().unwrap();
        p.bounds.max = p.bounds.min;
        p.validate().unwrap();

        assert!(matches!(
            GenParams::from_ron_str("(surface: ())"),
            Err(GenError::Parse(_))
        ));
        assert!(matches!(
            GenParams::load("does/not/exist.ron"),
            Err(GenError::Io { .. })
        ));
    }

    #[test]
    fn hash_is_stable_and_sensitive() {
        let a = phase1();
        assert_eq!(a.hash(), phase1().hash());

        // Formatting does not matter.
        let text = std::fs::read_to_string(PHASE1_PATH).unwrap();
        let reformatted = ron::ser::to_string(&a).unwrap();
        assert_ne!(text, reformatted);
        assert_eq!(
            GenParams::from_ron_str(&reformatted).unwrap().hash(),
            a.hash()
        );

        // Every field moves it.
        let edits: Vec<Edit> = vec![
            Box::new(|p| p.surface.base_height += 1),
            Box::new(|p| p.surface.amplitude += 1),
            Box::new(|p| p.surface.noise.frequency *= 1.5),
            Box::new(|p| p.surface.noise.octaves += 1),
            Box::new(|p| p.soil[0].material = "clay".into()),
            Box::new(|p| p.soil[0].min_cells = 0),
            Box::new(|p| p.soil[1].max_cells += 1),
            Box::new(|p| p.soil[1].noise.frequency = 0.5),
            Box::new(|p| p.soil.swap(0, 1)),
            Box::new(|p| {
                p.soil.pop();
            }),
            Box::new(|p| p.strata.boundary_amplitude += 1),
            Box::new(|p| p.strata.boundary_noise.octaves = 1),
            Box::new(|p| p.strata.layers[0].material = "granite".into()),
            Box::new(|p| p.strata.layers[1].until_depth = Some(57)),
            Box::new(|p| p.caves.surface_margin += 1),
            Box::new(|p| p.caves.blob.threshold += 0.01),
            Box::new(|p| p.caves.blob.noise.frequency *= 2.0),
            Box::new(|p| p.caves.worm.radius += 0.01),
            Box::new(|p| p.caves.worm.a.octaves += 1),
            Box::new(|p| p.caves.worm.b.frequency *= 0.5),
            Box::new(|p| std::mem::swap(&mut p.caves.worm.a, &mut p.caves.worm.b)),
            Box::new(|p| p.ores[0].material = "iron_ore".into()),
            Box::new(|p| p.ores[0].min_depth += 1),
            Box::new(|p| p.ores[1].max_depth += 1),
            Box::new(|p| p.ores[2].threshold += 0.01),
            Box::new(|p| p.ores[2].noise.frequency *= 2.0),
            Box::new(|p| p.ores.swap(0, 1)),
            Box::new(|p| {
                p.ores.pop();
            }),
            Box::new(|p| p.crystals.material = "stone".into()),
            Box::new(|p| p.crystals.chance_per_million += 1),
            Box::new(|p| p.bounds.min[0] -= 1),
            Box::new(|p| p.bounds.max[1] += 1),
            Box::new(|p| p.bedrock.material = "basalt".into()),
            Box::new(|p| p.bedrock.floor_cells += 1),
            Box::new(|p| p.bedrock.wall_cells += 1),
            Box::new(|p| p.magma_chambers.as_mut().unwrap().chamber_max_y += 1),
            Box::new(|p| p.magma_chambers = None),
        ];
        let mut seen = vec![a.hash()];
        for (i, edit) in edits.iter().enumerate() {
            let mut p = phase1();
            edit(&mut p);
            assert!(!seen.contains(&p.hash()), "edit {i} did not move the hash");
            seen.push(p.hash());
        }
    }
}
