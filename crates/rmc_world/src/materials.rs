//! Material definitions loaded from `assets/materials.ron` (SAD §6).
//!
//! Host and client load the same table and compare [`MaterialTable::hash`].
//! Code filters materials on `phase` and `tags`; it never branches on a
//! specific id or key.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Material index stored in every cell. `0` is air and is reserved.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct MaterialId(pub u16);

impl MaterialId {
    /// The reserved empty material.
    pub const AIR: MaterialId = MaterialId(0);
}

/// Physical phase. Collision treats `Solid` as blocking. Raycast and the
/// opaque mesh pass treat `Solid` and `Liquid` alike (SAD-P2 §5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Phase {
    Air,
    Solid,
    /// Water and magma (Phase 2). Not solid for collision; hit by raycasts;
    /// a translucent liquid is meshed in the client's liquid pass, an opaque
    /// one (magma) in the opaque pass.
    Liquid,
}

/// Default `conductivity` (SAD-P2 §5).
pub const DEFAULT_CONDUCTIVITY: u8 = 16;
/// Largest accepted `conductivity` (SAD-P2 §3).
pub const MAX_CONDUCTIVITY: u8 = 64;
/// Default `heat_capacity` (SAD-P2 §5).
pub const DEFAULT_HEAT_CAPACITY: u8 = 16;
/// Accepted `heat_capacity` range; the value must also be a power of two.
pub const HEAT_CAPACITY_RANGE: std::ops::RangeInclusive<u8> = 8..=128;

fn default_conductivity() -> u8 {
    DEFAULT_CONDUCTIVITY
}

fn default_heat_capacity() -> u8 {
    DEFAULT_HEAT_CAPACITY
}

/// One row of the material table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialDef {
    pub id: MaterialId,
    /// Stable name, e.g. `"granite"`.
    pub key: String,
    pub phase: Phase,
    /// `"stone"`, `"soil"`, `"ore"`, `"crystal"`, `"placeable"`, ...
    pub tags: Vec<String>,
    /// Damage needed to break; 0 = instant.
    pub hardness: u16,
    /// False for bedrock.
    pub diggable: bool,
    /// Items of this material produced on break (0 = none).
    pub yield_count: u8,
    /// Vertex color, RGBA.
    pub color: [u8; 4],
    /// Emissive strength, `0.0..`.
    pub emissive: f32,

    // Phase 2 fields (SAD-P2 §5). All have serde defaults, so the Phase 1
    // table loads unchanged.
    /// Plants: the controller and light treat passable solids as air.
    #[serde(default)]
    pub passable: bool,
    /// Heat conductivity, at most [`MAX_CONDUCTIVITY`].
    #[serde(default = "default_conductivity")]
    pub conductivity: u8,
    /// Heat capacity, a power of two in [`HEAT_CAPACITY_RANGE`].
    #[serde(default = "default_heat_capacity")]
    pub heat_capacity: u8,
    #[serde(default)]
    pub permeability: u8,
    /// Absolute moisture a porous solid holds.
    #[serde(default)]
    pub field_capacity: u8,
    /// Magma attempts a move only on iterations where `iter % (viscosity + 1) == 0`.
    #[serde(default)]
    pub viscosity: u8,
    #[serde(default)]
    pub weathering_rate: u8,
    /// Material key this becomes when its regolith reaches 255.
    #[serde(default)]
    pub weathers_to: Option<String>,
    /// Loaded, unused in Phase 2.
    #[serde(default)]
    pub erodibility: u8,
    /// Substance key, total released over a full 0..255 regolith cycle.
    #[serde(default)]
    pub composition: Vec<(String, u8)>,
    /// Gas key, amount per sub-step while exposed to air.
    #[serde(default)]
    pub emits: Vec<(String, u8)>,
    /// Clamped temperature in 0.1 °C.
    #[serde(default)]
    pub heat_source: Option<i16>,
    /// `None` takes the phase default; read it through
    /// [`MaterialDef::light_attenuation`].
    #[serde(default)]
    pub light_attenuation: Option<u8>,
    /// Plants may root on this material.
    #[serde(default)]
    pub anchor: bool,
    /// Material key yielded on break instead of this material.
    #[serde(default)]
    pub yield_material: Option<String>,
}

impl MaterialDef {
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    pub fn is_solid(&self) -> bool {
        self.phase == Phase::Solid
    }

    /// `Solid` or `Liquid`: what raycasts hit.
    pub fn is_solid_or_liquid(&self) -> bool {
        self.phase != Phase::Air
    }

    /// Whether the cell can be a column's `surface_y` (SAD-P2 §3): `Liquid`,
    /// or `Solid` and not `passable`.
    pub fn forms_surface(&self) -> bool {
        match self.phase {
            Phase::Liquid => true,
            Phase::Solid => !self.passable,
            Phase::Air => false,
        }
    }

    /// Light lost entering this cell: the table value, else 0 for air and
    /// passable solids and 255 otherwise (SAD-P2 §5).
    pub fn light_attenuation(&self) -> u8 {
        self.light_attenuation
            .unwrap_or(if self.phase == Phase::Air || self.passable {
                0
            } else {
                u8::MAX
            })
    }

    /// True when every Phase 2 field holds its default, so the Phase 1
    /// canonical encoding describes the definition completely.
    fn phase2_is_default(&self) -> bool {
        !self.passable
            && self.conductivity == DEFAULT_CONDUCTIVITY
            && self.heat_capacity == DEFAULT_HEAT_CAPACITY
            && self.permeability == 0
            && self.field_capacity == 0
            && self.viscosity == 0
            && self.weathering_rate == 0
            && self.weathers_to.is_none()
            && self.erodibility == 0
            && self.composition.is_empty()
            && self.emits.is_empty()
            && self.heat_source.is_none()
            && self.light_attenuation.is_none()
            && !self.anchor
            && self.yield_material.is_none()
    }
}

#[derive(Debug, Error)]
pub enum MaterialError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing materials RON: {0}")]
    Parse(#[from] ron::error::SpannedError),
    #[error("material table is empty")]
    Empty,
    #[error("material table has {0} entries; at most 65536 fit in a u16 id")]
    TooMany(usize),
    #[error("entry {index} has id {id}; ids must equal their position (dense, starting at 0)")]
    IdMismatch { index: usize, id: u16 },
    #[error("id 0 is reserved for air and must have phase Air")]
    AirNotFirst,
    #[error("material {id} has an empty key")]
    EmptyKey { id: u16 },
    #[error("duplicate material key {key:?}")]
    DuplicateKey { key: String },
    #[error("material {key:?} has an empty or duplicate tag {tag:?}")]
    BadTag { key: String, tag: String },
    #[error("material {key:?} has emissive {value}; it must be finite and >= 0")]
    BadEmissive { key: String, value: f32 },
    #[error("material {key:?} has conductivity {value}; at most {MAX_CONDUCTIVITY}")]
    BadConductivity { key: String, value: u8 },
    #[error("material {key:?} has heat_capacity {value}; it must be a power of two in 8..=128")]
    BadHeatCapacity { key: String, value: u8 },
    #[error("material {key:?} field {field} names unknown material {target:?}")]
    UnknownMaterial {
        key: String,
        field: &'static str,
        target: String,
    },
    #[error("material {key:?} field {field} has an empty or duplicate key {entry:?}")]
    BadEntry {
        key: String,
        field: &'static str,
        entry: String,
    },
}

/// The validated material table, indexed by [`MaterialId`].
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialTable {
    defs: Vec<MaterialDef>,
    by_key: BTreeMap<String, MaterialId>,
    hash: u64,
}

impl MaterialTable {
    /// Loads and validates a table from a RON file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, MaterialError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| MaterialError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let table = Self::from_ron_str(&text)?;
        tracing::info!(
            path = %path.display(),
            count = table.len(),
            hash = format_args!("{:016x}", table.hash),
            "material table loaded"
        );
        Ok(table)
    }

    /// Parses and validates a table from RON text: a list of [`MaterialDef`].
    pub fn from_ron_str(text: &str) -> Result<Self, MaterialError> {
        Self::from_defs(ron::from_str(text)?)
    }

    /// Validates a list of definitions and computes the table hash.
    pub fn from_defs(defs: Vec<MaterialDef>) -> Result<Self, MaterialError> {
        if defs.is_empty() {
            return Err(MaterialError::Empty);
        }
        if defs.len() > usize::from(u16::MAX) + 1 {
            return Err(MaterialError::TooMany(defs.len()));
        }
        let mut by_key = BTreeMap::new();
        for (index, d) in defs.iter().enumerate() {
            if usize::from(d.id.0) != index {
                return Err(MaterialError::IdMismatch { index, id: d.id.0 });
            }
            if d.id == MaterialId::AIR && d.phase != Phase::Air {
                return Err(MaterialError::AirNotFirst);
            }
            if d.key.is_empty() {
                return Err(MaterialError::EmptyKey { id: d.id.0 });
            }
            for (i, tag) in d.tags.iter().enumerate() {
                if tag.is_empty() || d.tags[..i].contains(tag) {
                    return Err(MaterialError::BadTag {
                        key: d.key.clone(),
                        tag: tag.clone(),
                    });
                }
            }
            if !d.emissive.is_finite() || d.emissive < 0.0 {
                return Err(MaterialError::BadEmissive {
                    key: d.key.clone(),
                    value: d.emissive,
                });
            }
            if by_key.insert(d.key.clone(), d.id).is_some() {
                return Err(MaterialError::DuplicateKey { key: d.key.clone() });
            }
            validate_phase2(d)?;
        }
        for d in &defs {
            let refs = [
                ("weathers_to", &d.weathers_to),
                ("yield_material", &d.yield_material),
            ];
            for (field, target) in refs {
                if let Some(target) = target
                    && !by_key.contains_key(target)
                {
                    return Err(MaterialError::UnknownMaterial {
                        key: d.key.clone(),
                        field,
                        target: target.clone(),
                    });
                }
            }
        }
        let hash = canonical_hash(&defs);
        Ok(Self { defs, by_key, hash })
    }

    /// xxh3 over the canonical encoding of every definition, in id order.
    /// Independent of RON formatting.
    pub fn hash(&self) -> u64 {
        self.hash
    }

    /// Looks up a material by key. Tests and data loaders use this; game
    /// logic filters on tags and phase instead.
    pub fn id(&self, key: &str) -> Option<MaterialId> {
        self.by_key.get(key).copied()
    }

    pub fn get(&self, id: MaterialId) -> Option<&MaterialDef> {
        self.defs.get(usize::from(id.0))
    }

    /// Definitions carrying `tag`, in id order.
    pub fn with_tag<'s, 't>(
        &'s self,
        tag: &'t str,
    ) -> impl Iterator<Item = &'s MaterialDef> + use<'s, 't> {
        self.defs.iter().filter(move |d| d.has_tag(tag))
    }

    /// True when `id` exists and carries `tag`.
    pub fn has_tag(&self, id: MaterialId, tag: &str) -> bool {
        self.get(id).is_some_and(|d| d.has_tag(tag))
    }

    /// All definitions in id order.
    pub fn iter(&self) -> impl Iterator<Item = &MaterialDef> {
        self.defs.iter()
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

/// Phase 2 limits on one definition (SAD-P2 §3, §5). Substance and gas keys
/// are checked by `rmc_eco`, which owns those tables.
fn validate_phase2(d: &MaterialDef) -> Result<(), MaterialError> {
    if d.conductivity > MAX_CONDUCTIVITY {
        return Err(MaterialError::BadConductivity {
            key: d.key.clone(),
            value: d.conductivity,
        });
    }
    if !d.heat_capacity.is_power_of_two() || !HEAT_CAPACITY_RANGE.contains(&d.heat_capacity) {
        return Err(MaterialError::BadHeatCapacity {
            key: d.key.clone(),
            value: d.heat_capacity,
        });
    }
    for (field, list) in [("composition", &d.composition), ("emits", &d.emits)] {
        for (i, (entry, _)) in list.iter().enumerate() {
            if entry.is_empty() || list[..i].iter().any(|(k, _)| k == entry) {
                return Err(MaterialError::BadEntry {
                    key: d.key.clone(),
                    field,
                    entry: entry.clone(),
                });
            }
        }
    }
    Ok(())
}

fn put_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn put_opt_str(buf: &mut Vec<u8>, s: &Option<String>) {
    match s {
        None => buf.push(0),
        Some(s) => {
            buf.push(1);
            put_str(buf, s);
        }
    }
}

fn put_pairs(buf: &mut Vec<u8>, pairs: &[(String, u8)]) {
    buf.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
    for (k, v) in pairs {
        put_str(buf, k);
        buf.push(*v);
    }
}

/// Fixed little-endian encoding of every field, strings length-prefixed, so
/// the hash depends only on table content.
///
/// The Phase 2 fields are appended as a second section only when some
/// definition sets one away from its default, so a Phase 1 table keeps the
/// hash it had before those fields existed.
fn canonical_hash(defs: &[MaterialDef]) -> u64 {
    let mut buf = Vec::with_capacity(64 * defs.len());
    buf.extend_from_slice(b"rmc.materials.v1");
    buf.extend_from_slice(&(defs.len() as u32).to_le_bytes());
    for d in defs {
        buf.extend_from_slice(&d.id.0.to_le_bytes());
        put_str(&mut buf, &d.key);
        buf.push(match d.phase {
            Phase::Air => 0,
            Phase::Solid => 1,
            Phase::Liquid => 2,
        });
        buf.extend_from_slice(&(d.tags.len() as u32).to_le_bytes());
        for tag in &d.tags {
            put_str(&mut buf, tag);
        }
        buf.extend_from_slice(&d.hardness.to_le_bytes());
        buf.push(u8::from(d.diggable));
        buf.push(d.yield_count);
        buf.extend_from_slice(&d.color);
        buf.extend_from_slice(&d.emissive.to_bits().to_le_bytes());
    }
    if !defs.iter().all(MaterialDef::phase2_is_default) {
        buf.extend_from_slice(b"rmc.materials.p2");
        for d in defs {
            buf.push(u8::from(d.passable));
            buf.push(d.conductivity);
            buf.push(d.heat_capacity);
            buf.push(d.permeability);
            buf.push(d.field_capacity);
            buf.push(d.viscosity);
            buf.push(d.weathering_rate);
            put_opt_str(&mut buf, &d.weathers_to);
            buf.push(d.erodibility);
            put_pairs(&mut buf, &d.composition);
            put_pairs(&mut buf, &d.emits);
            match d.heat_source {
                None => buf.push(0),
                Some(t) => {
                    buf.push(1);
                    buf.extend_from_slice(&t.to_le_bytes());
                }
            }
            match d.light_attenuation {
                None => buf.push(0),
                Some(a) => buf.extend_from_slice(&[1, a]),
            }
            buf.push(u8::from(d.anchor));
            put_opt_str(&mut buf, &d.yield_material);
        }
    }
    xxhash_rust::xxh3::xxh3_64(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHASE1_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    /// Rows of the shipped table that Phase 1 defined (ids `0..13`); Phase 2
    /// appends its materials after them.
    const PHASE1_ROWS: usize = 13;

    /// The Phase 1 rows of the shipped table.
    fn phase1() -> MaterialTable {
        let t = MaterialTable::load(PHASE1_PATH).expect("assets/materials.ron loads");
        let rows = t.iter().take(PHASE1_ROWS).cloned().collect();
        MaterialTable::from_defs(rows).expect("Phase 1 rows form a table")
    }

    /// Small fixed table for hash and validation tests, independent of the
    /// shipped asset so asset tuning does not move the golden hash.
    const FIXTURE: &str = r#"[
        (id: 0, key: "air", phase: Air, tags: [], hardness: 0, diggable: false,
         yield_count: 0, color: (0, 0, 0, 0), emissive: 0.0),
        (id: 1, key: "rock", phase: Solid, tags: ["stone"], hardness: 30, diggable: true,
         yield_count: 1, color: (128, 128, 128, 255), emissive: 0.0),
        (id: 2, key: "shine", phase: Solid, tags: ["crystal", "placeable"], hardness: 20,
         diggable: true, yield_count: 2, color: (90, 200, 255, 255), emissive: 2.5),
    ]"#;

    #[test]
    fn loads_phase1_table() {
        let t = phase1();
        let keys: Vec<&str> = t.iter().map(|d| d.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "air",
                "topsoil",
                "dirt",
                "clay",
                "stone",
                "granite",
                "basalt",
                "coal_seam",
                "copper_ore",
                "iron_ore",
                "glowcrystal",
                "bedrock",
                "brass_plate",
            ]
        );
        for (i, d) in t.iter().enumerate() {
            assert_eq!(d.id, MaterialId(i as u16), "{} id", d.key);
            assert_eq!(t.get(d.id), Some(d));
        }

        let air = t.get(MaterialId::AIR).unwrap();
        assert_eq!(air.key, "air");
        assert_eq!(air.phase, Phase::Air);
        assert!(!air.is_solid());
        assert_eq!(air.yield_count, 0);
        for d in t.iter().skip(1) {
            assert!(d.is_solid(), "{} is solid", d.key);
            assert_eq!(d.color[3], 255, "{} is opaque", d.key);
        }

        let get = |k: &str| t.get(t.id(k).unwrap()).unwrap();
        assert!(!get("bedrock").diggable);
        assert_eq!(get("bedrock").yield_count, 0);
        for d in t.iter().skip(1).filter(|d| d.key != "bedrock") {
            assert!(d.diggable, "{} diggable", d.key);
            assert!(d.hardness > 0, "{} hardness", d.key);
            assert!(d.yield_count > 0, "{} yields", d.key);
        }
        assert!(get("glowcrystal").emissive > 0.0);
        for d in t.iter().filter(|d| d.key != "glowcrystal") {
            assert_eq!(d.emissive, 0.0, "{} not emissive", d.key);
        }
        assert!(get("granite").hardness > get("stone").hardness);
        assert!(get("basalt").hardness > get("granite").hardness);

        let tagged = |tag: &str| -> Vec<&str> { t.with_tag(tag).map(|d| d.key.as_str()).collect() };
        assert_eq!(tagged("soil"), ["topsoil", "dirt", "clay"]);
        assert_eq!(tagged("stone"), ["stone", "granite", "basalt"]);
        assert_eq!(tagged("ore"), ["coal_seam", "copper_ore", "iron_ore"]);
        assert_eq!(tagged("crystal"), ["glowcrystal"]);
        assert_eq!(tagged("placeable"), ["brass_plate"]);
    }

    #[test]
    fn hash_is_stable() {
        let a = MaterialTable::from_ron_str(FIXTURE).unwrap();
        let b = MaterialTable::from_ron_str(FIXTURE).unwrap();
        assert_eq!(a.hash(), b.hash());

        // Golden value: the canonical encoding must not drift between builds,
        // machines or crate upgrades, because saves and Hello carry it.
        assert_eq!(a.hash(), 0x839f_6f47_6eb4_5041, "golden hash moved");

        // Formatting and comments do not matter.
        let reformatted = FIXTURE
            .replace("\n", " ")
            .replace("        ", " ")
            .replacen("[", "[ // materials\n", 1);
        let c = MaterialTable::from_ron_str(&reformatted).unwrap();
        assert_eq!(c.hash(), a.hash());

        // Round trip through the defs gives the same hash.
        let d = MaterialTable::from_defs(a.iter().cloned().collect()).unwrap();
        assert_eq!(d.hash(), a.hash());

        // Every field contributes.
        let base: Vec<MaterialDef> = a.iter().cloned().collect();
        let edits: [fn(&mut MaterialDef); 8] = [
            |d| d.key.push('x'),
            |d| d.tags.push("ore".into()),
            |d| d.hardness += 1,
            |d| d.diggable = !d.diggable,
            |d| d.yield_count += 1,
            |d| d.color[0] ^= 1,
            |d| d.emissive += 0.25,
            |d| d.tags[0] = "soil".into(),
        ];
        for (i, edit) in edits.iter().enumerate() {
            let mut defs = base.clone();
            edit(&mut defs[1]);
            let changed = MaterialTable::from_defs(defs).unwrap();
            assert_ne!(changed.hash(), a.hash(), "edit {i} did not change the hash");
        }
        // Tag boundaries are encoded: ["ab"] differs from ["a", "b"].
        let mut x = base.clone();
        x[1].tags = vec!["ab".into()];
        let mut y = base.clone();
        y[1].tags = vec!["a".into(), "b".into()];
        assert_ne!(
            MaterialTable::from_defs(x).unwrap().hash(),
            MaterialTable::from_defs(y).unwrap().hash()
        );

        // The shipped table hashes the same on every load.
        assert_eq!(phase1().hash(), phase1().hash());
    }

    #[test]
    fn lookup_by_key_and_tag() {
        let t = phase1();
        let granite = t.id("granite").unwrap();
        assert_eq!(t.get(granite).unwrap().key, "granite");
        assert_eq!(t.id("air"), Some(MaterialId::AIR));
        assert_eq!(t.id("unobtainium"), None);
        assert_eq!(t.id("Granite"), None, "keys are case sensitive");
        assert_eq!(t.get(MaterialId(t.len() as u16)), None);

        assert!(t.has_tag(granite, "stone"));
        assert!(!t.has_tag(granite, "ore"));
        assert!(!t.has_tag(MaterialId(u16::MAX), "stone"));
        assert_eq!(t.with_tag("no_such_tag").count(), 0);
        let stone_ids: Vec<MaterialId> = t.with_tag("stone").map(|d| d.id).collect();
        assert!(stone_ids.windows(2).all(|w| w[0] < w[1]), "id order");
        assert!(stone_ids.contains(&granite));

        // Validation rejects malformed tables.
        let f = MaterialTable::from_ron_str(FIXTURE).unwrap();
        let base: Vec<MaterialDef> = f.iter().cloned().collect();
        let reject = |defs: Vec<MaterialDef>| MaterialTable::from_defs(defs).unwrap_err();

        assert!(matches!(reject(vec![]), MaterialError::Empty));
        let mut d = base.clone();
        d[2].id = MaterialId(5);
        assert!(matches!(
            reject(d),
            MaterialError::IdMismatch { index: 2, id: 5 }
        ));
        let mut d = base.clone();
        d[0].phase = Phase::Solid;
        assert!(matches!(reject(d), MaterialError::AirNotFirst));
        let mut d = base.clone();
        d[2].key = "rock".into();
        assert!(matches!(reject(d), MaterialError::DuplicateKey { .. }));
        let mut d = base.clone();
        d[1].key.clear();
        assert!(matches!(reject(d), MaterialError::EmptyKey { id: 1 }));
        let mut d = base.clone();
        d[1].tags.push("stone".into());
        assert!(matches!(reject(d), MaterialError::BadTag { .. }));
        let mut d = base.clone();
        d[1].tags.push(String::new());
        assert!(matches!(reject(d), MaterialError::BadTag { .. }));
        let mut d = base.clone();
        d[2].emissive = -1.0;
        assert!(matches!(reject(d), MaterialError::BadEmissive { .. }));
        let mut d = base.clone();
        d[2].emissive = f32::NAN;
        assert!(matches!(reject(d), MaterialError::BadEmissive { .. }));

        // Unknown fields are a parse error, not silently ignored.
        let typo = FIXTURE.replacen("hardness: 30", "hardnes: 30", 1);
        assert!(matches!(
            MaterialTable::from_ron_str(&typo),
            Err(MaterialError::Parse(_))
        ));
        assert!(matches!(
            MaterialTable::load("does/not/exist.ron"),
            Err(MaterialError::Io { .. })
        ));
    }

    #[test]
    fn phase1_table_loads_with_phase2_defaults() {
        // A Phase 1 table has no Phase 2 fields; every one takes its SAD-P2
        // §5 default. The shipped table no longer qualifies: Phase 2 tunes
        // its rows (SAD-P2 §4, §7.1), so the fixture is the Phase 1 text.
        let t = MaterialTable::from_ron_str(FIXTURE).unwrap();
        for d in t.iter() {
            assert_ne!(d.phase, Phase::Liquid, "{}", d.key);
            assert!(!d.passable, "{} passable", d.key);
            assert_eq!(d.conductivity, DEFAULT_CONDUCTIVITY, "{}", d.key);
            assert_eq!(d.heat_capacity, DEFAULT_HEAT_CAPACITY, "{}", d.key);
            assert_eq!(d.permeability, 0, "{}", d.key);
            assert_eq!(d.field_capacity, 0, "{}", d.key);
            assert_eq!(d.viscosity, 0, "{}", d.key);
            assert_eq!(d.weathering_rate, 0, "{}", d.key);
            assert_eq!(d.weathers_to, None, "{}", d.key);
            assert_eq!(d.erodibility, 0, "{}", d.key);
            assert!(d.composition.is_empty(), "{}", d.key);
            assert!(d.emits.is_empty(), "{}", d.key);
            assert_eq!(d.heat_source, None, "{}", d.key);
            assert_eq!(d.light_attenuation, None, "{}", d.key);
            assert!(!d.anchor, "{}", d.key);
            assert_eq!(d.yield_material, None, "{}", d.key);
            let expect = if d.phase == Phase::Air { 0 } else { u8::MAX };
            assert_eq!(d.light_attenuation(), expect, "{} attenuation", d.key);
            assert_eq!(d.is_solid_or_liquid(), d.is_solid(), "{}", d.key);
        }
        assert_eq!(DEFAULT_CONDUCTIVITY, 16);
        assert_eq!(DEFAULT_HEAT_CAPACITY, 16);
        // Defaults leave the Phase 1 encoding, and so its hash, unchanged:
        // the golden value in `hash_is_stable` is the proof for the fixture.
        let f = MaterialTable::from_ron_str(FIXTURE).unwrap();
        assert_eq!(f.hash(), 0x839f_6f47_6eb4_5041);

        // Phase 2 rows: Liquid parses, fields override their defaults.
        let close = FIXTURE.rfind(']').unwrap();
        let p2 = format!(
            "{}{}",
            &FIXTURE[..close],
            r#"
            (id: 3, key: "wet", phase: Liquid, tags: [], hardness: 0, diggable: true,
             yield_count: 0, color: (40, 90, 200, 160), emissive: 0.0,
             conductivity: 64, heat_capacity: 8, light_attenuation: Some(32)),
            (id: 4, key: "moss", phase: Solid, tags: ["plant"], hardness: 1, diggable: true,
             yield_count: 1, color: (60, 140, 60, 255), emissive: 0.0, passable: true,
             permeability: 96, field_capacity: 48, viscosity: 3, weathering_rate: 2,
             weathers_to: Some("rock"), erodibility: 5, composition: [("c", 40)],
             emits: [("smoke", 1)], heat_source: Some(1300), anchor: true,
             yield_material: Some("shine")),
        ]"#
        );
        let t2 = MaterialTable::from_ron_str(&p2).unwrap();
        let wet = t2.get(t2.id("wet").unwrap()).unwrap();
        assert_eq!(wet.phase, Phase::Liquid);
        assert!(!wet.is_solid(), "liquid is not solid for collision");
        assert!(wet.is_solid_or_liquid(), "liquid is hit and meshed");
        assert_eq!((wet.conductivity, wet.heat_capacity), (64, 8));
        assert_eq!(wet.light_attenuation(), 32);
        let moss = t2.get(t2.id("moss").unwrap()).unwrap();
        assert!(moss.passable && moss.anchor);
        assert_eq!(moss.light_attenuation(), 0, "passable defaults to 0");
        assert_eq!(moss.weathers_to.as_deref(), Some("rock"));
        assert_eq!(moss.yield_material.as_deref(), Some("shine"));
        assert_eq!(moss.composition, [("c".to_string(), 40)]);
        assert_eq!(moss.emits, [("smoke".to_string(), 1)]);
        assert_eq!(moss.heat_source, Some(1300));
        assert_ne!(t2.hash(), f.hash());

        // Every Phase 2 field contributes to the hash.
        let base: Vec<MaterialDef> = t2.iter().cloned().collect();
        let edits: [fn(&mut MaterialDef); 16] = [
            |d| d.phase = Phase::Liquid,
            |d| d.passable = !d.passable,
            |d| d.conductivity -= 1,
            |d| d.heat_capacity *= 2,
            |d| d.permeability += 1,
            |d| d.field_capacity += 1,
            |d| d.viscosity += 1,
            |d| d.weathering_rate += 1,
            |d| d.weathers_to = None,
            |d| d.erodibility += 1,
            |d| d.composition[0].1 += 1,
            |d| d.emits[0].0.push('x'),
            |d| d.heat_source = Some(1),
            |d| d.light_attenuation = Some(7),
            |d| d.anchor = !d.anchor,
            |d| d.yield_material = None,
        ];
        let h = MaterialTable::from_defs(base.clone()).unwrap().hash();
        for (i, edit) in edits.iter().enumerate() {
            let mut defs = base.clone();
            edit(&mut defs[4]);
            let changed = MaterialTable::from_defs(defs).unwrap();
            assert_ne!(
                changed.hash(),
                h,
                "phase 2 edit {i} did not change the hash"
            );
        }
        // A single non-default field on a Phase 1 table changes its hash.
        let mut defs: Vec<MaterialDef> = f.iter().cloned().collect();
        defs[1].permeability = 1;
        assert_ne!(MaterialTable::from_defs(defs).unwrap().hash(), f.hash());

        // Validation of the Phase 2 limits (SAD-P2 §3, §5).
        let reject = |edit: fn(&mut MaterialDef)| {
            let mut defs = base.clone();
            edit(&mut defs[3]);
            MaterialTable::from_defs(defs).unwrap_err()
        };
        let accept = |edit: fn(&mut MaterialDef)| {
            let mut defs = base.clone();
            edit(&mut defs[3]);
            MaterialTable::from_defs(defs).unwrap();
        };
        accept(|d| d.conductivity = 64);
        accept(|d| d.conductivity = 0);
        assert!(matches!(
            reject(|d| d.conductivity = 65),
            MaterialError::BadConductivity { value: 65, .. }
        ));
        assert!(matches!(
            reject(|d| d.conductivity = u8::MAX),
            MaterialError::BadConductivity { .. }
        ));
        for c in [8, 16, 32, 64, 128] {
            let mut defs = base.clone();
            defs[3].heat_capacity = c;
            MaterialTable::from_defs(defs).unwrap_or_else(|e| panic!("capacity {c}: {e}"));
        }
        for c in [0, 1, 2, 4, 7, 12, 24, 100, 129, u8::MAX] {
            let mut defs = base.clone();
            defs[3].heat_capacity = c;
            assert!(
                matches!(
                    MaterialTable::from_defs(defs),
                    Err(MaterialError::BadHeatCapacity { .. })
                ),
                "capacity {c} accepted"
            );
        }
        assert!(matches!(
            reject(|d| d.weathers_to = Some("nope".into())),
            MaterialError::UnknownMaterial { .. }
        ));
        assert!(matches!(
            reject(|d| d.yield_material = Some("nope".into())),
            MaterialError::UnknownMaterial { .. }
        ));
        assert!(matches!(
            reject(|d| d.composition = vec![("c".into(), 1), ("c".into(), 2)]),
            MaterialError::BadEntry { .. }
        ));
        assert!(matches!(
            reject(|d| d.emits = vec![(String::new(), 1)]),
            MaterialError::BadEntry { .. }
        ));
        // Air must stay Air, never Liquid.
        let mut defs = base.clone();
        defs[0].phase = Phase::Liquid;
        assert!(matches!(
            MaterialTable::from_defs(defs),
            Err(MaterialError::AirNotFirst)
        ));
        // Unknown Phase 2 field names are still rejected.
        let typo = p2.replacen("permeability: 96", "permeabilty: 96", 1);
        assert!(matches!(
            MaterialTable::from_ron_str(&typo),
            Err(MaterialError::Parse(_))
        ));
    }
}
