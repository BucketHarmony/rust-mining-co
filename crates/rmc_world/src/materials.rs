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

/// Physical phase. Rendering, collision and raycast treat `Solid` as blocking.
/// `Liquid` is reserved for a later phase and is not a variant yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Phase {
    Air,
    Solid,
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
}

impl MaterialDef {
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    pub fn is_solid(&self) -> bool {
        self.phase == Phase::Solid
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

/// Fixed little-endian encoding of every field, strings length-prefixed, so
/// the hash depends only on table content.
fn canonical_hash(defs: &[MaterialDef]) -> u64 {
    fn put_str(buf: &mut Vec<u8>, s: &str) {
        buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
        buf.extend_from_slice(s.as_bytes());
    }
    let mut buf = Vec::with_capacity(64 * defs.len());
    buf.extend_from_slice(b"rmc.materials.v1");
    buf.extend_from_slice(&(defs.len() as u32).to_le_bytes());
    for d in defs {
        buf.extend_from_slice(&d.id.0.to_le_bytes());
        put_str(&mut buf, &d.key);
        buf.push(match d.phase {
            Phase::Air => 0,
            Phase::Solid => 1,
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
    xxhash_rust::xxh3::xxh3_64(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHASE1_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    fn phase1() -> MaterialTable {
        MaterialTable::load(PHASE1_PATH).expect("assets/materials.ron loads")
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
}
