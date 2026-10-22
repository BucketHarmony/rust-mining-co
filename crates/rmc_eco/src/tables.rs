//! Phase 2 data tables (SAD-P2 §4, §5, §7): substances, gases, transitions,
//! reactions, plants, seasons, and the constants and roles in `eco.ron`.
//!
//! Host and client load the same files. [`EcoTables::hash`] covers them and
//! the material table; the save header carries it as `tables_hash`. Pass
//! code reads behavior from these rows and from the `roles` section, never
//! from a material, gas or substance key of its own (rules 18, 19).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use rmc_world::EcoRules;
use rmc_world::layers::{LayerId, SubstanceLayers};
use rmc_world::materials::{MaterialError, MaterialId, MaterialTable, Phase};
use rmc_world::query::{PhRule, SubstanceTint, ToxProfile};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Layer counts live with the chunk layers in `rmc_world` (SAD-P2 §6).
pub use rmc_world::layers::{N_GASES, N_SUBSTANCES};
/// Upper end of the derived pH scale (SAD-P2 §7.6 clamps to `0..=14`).
pub use rmc_world::query::PH_MAX;
/// Largest accepted `*_shift` constant; keeps `x / 2^shift` inside `i32`.
pub const MAX_SHIFT: u8 = 30;
/// Largest accepted `seep_num`; the threshold is `field_capacity · seep_num / 256`.
pub const SEEP_DEN: u16 = 256;

/// Roles every `eco.ron` must name, by section (SAD-P2 §3).
pub const MATERIAL_ROLES: [&str; 6] = ["air", "water", "ice", "magma", "obsidian", "basalt"];
pub const GAS_ROLES: [&str; 4] = ["steam", "smoke", "firedamp", "chokedamp"];
pub const SUBSTANCE_ROLES: [&str; 3] = ["organic", "acid", "lime"];

pub const MATERIALS_FILE: &str = "materials.ron";
pub const SUBSTANCES_FILE: &str = "substances.ron";
pub const GASES_FILE: &str = "gases.ron";
pub const TRANSITIONS_FILE: &str = "transitions.ron";
pub const REACTIONS_FILE: &str = "reactions.ron";
pub const PLANTS_FILE: &str = "plants.ron";
pub const SEASONS_FILE: &str = "seasons.ron";
pub const ECO_FILE: &str = "eco.ron";

/// Whose toxicity profile [`EcoTables::tox_profile`] builds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToxKind {
    /// `tox_plant`.
    Plant,
    /// `tox_player`: the HUD's spoil indicator.
    Player,
}

/// One row of `substances.ron` (SAD-P2 §7.6). Row order is the layer index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubstanceDef {
    pub key: String,
    pub mobility: u8,
    /// Used instead of `mobility` when the donor's pH is below `acid_ph`.
    pub mobility_acid: u8,
    pub plant_nutrient: bool,
    #[serde(default)]
    pub tox_plant: Option<u8>,
    #[serde(default)]
    pub tox_player: Option<u8>,
    /// Material key a harvest of held amounts yields.
    #[serde(default)]
    pub yield_material: Option<String>,
}

/// One row of `gases.ron` (SAD-P2 §7.4). Row order is the layer index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GasDef {
    pub key: String,
    /// Negative rises (lighter than air), positive sinks; drift moves
    /// `|density| · 2^drift_shift` per visit.
    pub density: i8,
    pub diffusion: u8,
    pub flammable: bool,
    pub ignite_min: u8,
    pub ignite_radius_per_64: u8,
    pub visible: bool,
}

/// Which cells a transition applies to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaterialMatch {
    Key(String),
    Tag(String),
}

/// Temperature condition of a transition, in 0.1 °C.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionWhen {
    TempBelow(i16),
    TempAbove(i16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionEffect {
    /// The cell's fill becomes steam 1:1 (SAD-P2 §7.2).
    BoilAll,
}

/// One row of `transitions.ron` (SAD-P2 §7.2); first match per cell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionDef {
    pub from: MaterialMatch,
    pub to: String,
    pub when: TransitionWhen,
    #[serde(default)]
    pub effect: Option<TransitionEffect>,
}

/// Reaction conditions (SAD-P2 §7.6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Condition {
    Exposed,
    Submerged,
    PhBelow(u8),
    PhAbove(u8),
    Evaporating,
    Tag(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoatTarget {
    Floor,
}

/// Reaction side effects (SAD-P2 §7.6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SideEffect {
    WeatherBoost(u8),
    /// Target, crust material key, substance key, amount.
    Coat(CoatTarget, String, String, u8),
    /// Gas key, amount.
    EmitGas(String, u8),
}

/// One row of `reactions.ron` (SAD-P2 §7.6); first match per cell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReactionDef {
    pub id: String,
    pub consume: Vec<(String, u8)>,
    #[serde(rename = "where")]
    pub conditions: Vec<Condition>,
    #[serde(default)]
    pub produce: Vec<(String, u8)>,
    #[serde(default)]
    pub side: Option<SideEffect>,
}

/// What a plant needs (SAD-P2 §7.8): light and temperature at the plant
/// cell, moisture and substances at the anchor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlantNeeds {
    /// Minimum light.
    pub light: u8,
    /// Minimum anchor moisture.
    pub moisture: u8,
    /// Inclusive temperature range, 0.1 °C.
    pub temp: (i16, i16),
    #[serde(default)]
    pub substance_min: Vec<(String, u8)>,
    #[serde(default)]
    pub substance_max: Vec<(String, u8)>,
}

/// One row of `plants.ron` (SAD-P2 §7.8). Row order is the species index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlantDef {
    pub key: String,
    pub material: String,
    pub anchor_tags: Vec<String>,
    pub needs: PlantNeeds,
    pub growth_per_substep: u8,
    pub spread_chance: u8,
    pub spread_radius: u8,
    pub litter: u8,
    pub lifespan_seasons: u8,
    #[serde(default)]
    pub uptake: Vec<(String, u8)>,
    #[serde(default)]
    pub holds: bool,
    #[serde(default)]
    pub tall: bool,
}

/// One season of `seasons.ron`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeasonDef {
    pub key: String,
    /// Clamp for sky-open and surface cells, 0.1 °C.
    pub air_temp: i16,
    pub rain: u8,
}

/// `seasons.ron`: sub-steps per season and the season cycle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeasonTable {
    pub substeps: u8,
    pub seasons: Vec<SeasonDef>,
}

/// Values that replace a field of every season row
/// ([`EcoTables::with_season_override`]); `None` keeps the table's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeasonOverride {
    pub rain: Option<u8>,
    /// 0.1 °C.
    pub air_temp: Option<i16>,
}

/// Every named constant of SAD-P2 §7 and §8, under its SAD name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EcoParams {
    // §7.1 temperature, §8 profile
    pub heat_shift: u8,
    pub geothermal_base: i16,
    pub gradient: i16,
    // §7.2 transitions, §8 worldgen
    pub magma_init_temp: i16,
    // §7.3 water
    pub flow_iters: u8,
    pub flow_cap: u8,
    pub infil_shift: u8,
    pub infil_side_shift: u8,
    pub seep_num: u16,
    pub seep_shift: u8,
    pub condense_temp: i16,
    pub evap_table: Vec<u8>,
    /// Width of one `evap_table` bucket in 0.1 °C, counted up from 0 °C.
    pub evap_bucket: i16,
    // §7.4 gases
    pub drift_shift: u8,
    pub diff_shift: u8,
    // §7.5 weathering
    pub soft_regolith: u8,
    // §7.6 chemistry
    pub ph_shift: u8,
    pub acid_ph: u8,
    pub evap_low_fill: u8,
    pub crust_threshold: u8,
    // §7.7 light
    pub light_lateral_iters: u8,
    pub light_falloff: u8,
    // §7.8 plants
    pub stress_death: u8,
    pub harvest_shift: u8,
    /// §7.8 step 4: a `tall` plant whose growth is above this occupies the
    /// cell above as well. `None` (tables written before F165): tall plants
    /// stay one cell. Not encoded when `None`, so older tables keep their
    /// hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tall_growth: Option<u8>,
    // §7.9 activity
    pub quiet_skip: u8,
}

/// The `roles` section of `eco.ron`: role name to key, by kind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roles {
    pub materials: BTreeMap<String, String>,
    pub gases: BTreeMap<String, String>,
    pub substances: BTreeMap<String, String>,
}

/// Thresholds the E2E scenarios and budget tests read (`eco_test.*`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EcoTestParams {
    pub swale_min: u32,
    pub quiet_tick_dev_ms: u32,
}

/// One row of `eco.tint` (SAD-P2 §10): how a dissolved substance shifts
/// the color of water in the liquid pass. See [`SubstanceTint`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TintDef {
    pub substance: String,
    /// sRGB.
    pub color: [u8; 3],
    /// The amount at and above which the tint is at full strength; above 0.
    pub full: u8,
    /// The share of `color` at full strength, out of 255.
    pub strength: u8,
}

/// One row of `eco.gas_tint` (SAD-P2 §10): how a `visible` gas at the
/// camera's cell tints the screen. The client composites the rows in
/// order, each a layer of `color` whose opacity is
/// [`SubstanceTint::share`] of the concentration, out of 255.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GasTintDef {
    pub gas: String,
    /// sRGB.
    pub color: [u8; 3],
    /// The concentration at and above which the tint is at full strength;
    /// above 0.
    pub full: u8,
    /// The opacity at full strength, out of 255.
    pub strength: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EcoFile {
    params: EcoParams,
    roles: Roles,
    eco_test: EcoTestParams,
    /// Absent means no tint.
    #[serde(default)]
    tint: Vec<TintDef>,
    /// Absent means no gas tint.
    #[serde(default)]
    gas_tint: Vec<GasTintDef>,
}

/// The text of the seven Phase 2 files, for loading from memory.
#[derive(Clone, Debug, Default)]
pub struct EcoSources {
    pub substances: String,
    pub gases: String,
    pub transitions: String,
    pub reactions: String,
    pub plants: String,
    pub seasons: String,
    pub eco: String,
}

impl EcoSources {
    /// Reads the seven files from `dir`.
    pub fn read(dir: impl AsRef<Path>) -> Result<Self, EcoTableError> {
        let dir = dir.as_ref();
        let read = |file: &str| {
            let path = dir.join(file);
            std::fs::read_to_string(&path).map_err(|source| EcoTableError::Io {
                path: path.display().to_string(),
                source,
            })
        };
        Ok(Self {
            substances: read(SUBSTANCES_FILE)?,
            gases: read(GASES_FILE)?,
            transitions: read(TRANSITIONS_FILE)?,
            reactions: read(REACTIONS_FILE)?,
            plants: read(PLANTS_FILE)?,
            seasons: read(SEASONS_FILE)?,
            eco: read(ECO_FILE)?,
        })
    }
}

#[derive(Debug, Error)]
pub enum EcoTableError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {file}: {source}")]
    Parse {
        file: &'static str,
        #[source]
        source: ron::error::SpannedError,
    },
    #[error(transparent)]
    Materials(#[from] MaterialError),
    #[error("{file}: {msg}")]
    Invalid { file: &'static str, msg: String },
}

fn invalid(file: &'static str, msg: impl Into<String>) -> EcoTableError {
    EcoTableError::Invalid {
        file,
        msg: msg.into(),
    }
}

fn parse<T: serde::de::DeserializeOwned>(
    file: &'static str,
    text: &str,
) -> Result<T, EcoTableError> {
    ron::from_str(text).map_err(|source| EcoTableError::Parse { file, source })
}

/// The validated Phase 2 tables plus the material table they refer to.
#[derive(Clone, Debug, PartialEq)]
pub struct EcoTables {
    materials: Arc<MaterialTable>,
    substances: Vec<SubstanceDef>,
    /// The substance keys in row order: row `i` is `LayerId::Substance(i)`.
    substance_layers: SubstanceLayers,
    gases: Vec<GasDef>,
    transitions: Vec<TransitionDef>,
    reactions: Vec<ReactionDef>,
    plants: Vec<PlantDef>,
    seasons: SeasonTable,
    params: EcoParams,
    roles: Roles,
    eco_test: EcoTestParams,
    tint: Vec<TintDef>,
    gas_tint: Vec<GasTintDef>,
    unresolved: BTreeSet<String>,
    hash: u64,
}

impl EcoTables {
    /// Loads `materials.ron` and the seven Phase 2 files from `dir`.
    pub fn load(dir: impl AsRef<Path>) -> Result<Self, EcoTableError> {
        let dir = dir.as_ref();
        let materials = Arc::new(MaterialTable::load(dir.join(MATERIALS_FILE))?);
        Self::load_with_materials(materials, dir)
    }

    /// Loads the seven Phase 2 files from `dir` against an already loaded
    /// material table (the one the `World` holds).
    pub fn load_with_materials(
        materials: Arc<MaterialTable>,
        dir: impl AsRef<Path>,
    ) -> Result<Self, EcoTableError> {
        let tables = Self::from_sources(materials, &EcoSources::read(&dir)?)?;
        tracing::info!(
            dir = %dir.as_ref().display(),
            hash = format_args!("{:016x}", tables.hash),
            unresolved = tables.unresolved.len(),
            "eco tables loaded"
        );
        Ok(tables)
    }

    /// Parses, cross-validates and hashes the tables.
    ///
    /// Material keys named by the eco files may be missing from the material
    /// table: Phase 2 adds its materials feature by feature (SAD-P2 §15).
    /// They are listed in [`EcoTables::unresolved_materials`] and resolve to
    /// `None`. Substance, gas and tag references must be valid.
    pub fn from_sources(
        materials: Arc<MaterialTable>,
        src: &EcoSources,
    ) -> Result<Self, EcoTableError> {
        let substances: Vec<SubstanceDef> = parse(SUBSTANCES_FILE, &src.substances)?;
        let gases: Vec<GasDef> = parse(GASES_FILE, &src.gases)?;
        let transitions: Vec<TransitionDef> = parse(TRANSITIONS_FILE, &src.transitions)?;
        let reactions: Vec<ReactionDef> = parse(REACTIONS_FILE, &src.reactions)?;
        let plants: Vec<PlantDef> = parse(PLANTS_FILE, &src.plants)?;
        let seasons: SeasonTable = parse(SEASONS_FILE, &src.seasons)?;
        let EcoFile {
            params,
            roles,
            eco_test,
            tint,
            gas_tint,
        } = parse(ECO_FILE, &src.eco)?;

        let mut unresolved = BTreeSet::new();
        let mut material_ref = |key: &str| {
            if materials.id(key).is_none() {
                unresolved.insert(key.to_string());
            }
        };

        unique_keys(
            SUBSTANCES_FILE,
            "substance",
            substances.iter().map(|s| &s.key),
        )?;
        if substances.is_empty() || substances.len() > N_SUBSTANCES {
            return Err(invalid(
                SUBSTANCES_FILE,
                format!(
                    "{} substances; expected 1..={N_SUBSTANCES}",
                    substances.len()
                ),
            ));
        }
        for s in &substances {
            if let Some(m) = &s.yield_material {
                material_ref(m);
            }
        }
        let substance_layers = SubstanceLayers::new(substances.iter().map(|s| s.key.as_str()))
            .map_err(|e| invalid(SUBSTANCES_FILE, e.to_string()))?;
        let sub_keys: BTreeSet<&str> = substances.iter().map(|s| s.key.as_str()).collect();

        unique_keys(GASES_FILE, "gas", gases.iter().map(|g| &g.key))?;
        if gases.is_empty() || gases.len() > N_GASES {
            return Err(invalid(
                GASES_FILE,
                format!("{} gases; expected 1..={N_GASES}", gases.len()),
            ));
        }
        for g in &gases {
            if g.flammable && g.ignite_min == 0 {
                return Err(invalid(
                    GASES_FILE,
                    format!("flammable gas {:?} has ignite_min 0", g.key),
                ));
            }
        }
        let gas_keys: BTreeSet<&str> = gases.iter().map(|g| g.key.as_str()).collect();

        for d in materials.iter() {
            pairs_in(
                MATERIALS_FILE,
                &d.key,
                "composition",
                &d.composition,
                &sub_keys,
            )?;
            pairs_in(MATERIALS_FILE, &d.key, "emits", &d.emits, &gas_keys)?;
        }

        for t in &transitions {
            match &t.from {
                MaterialMatch::Key(k) => material_ref(k),
                MaterialMatch::Tag(tag) => non_empty(TRANSITIONS_FILE, "from tag", tag)?,
            }
            material_ref(&t.to);
        }

        unique_keys(REACTIONS_FILE, "reaction", reactions.iter().map(|r| &r.id))?;
        for r in &reactions {
            if r.consume.is_empty() {
                return Err(invalid(
                    REACTIONS_FILE,
                    format!("reaction {:?} consumes nothing", r.id),
                ));
            }
            pairs_in(REACTIONS_FILE, &r.id, "consume", &r.consume, &sub_keys)?;
            pairs_in(REACTIONS_FILE, &r.id, "produce", &r.produce, &sub_keys)?;
            for c in &r.conditions {
                match c {
                    Condition::PhBelow(n) | Condition::PhAbove(n) if *n > PH_MAX => {
                        return Err(invalid(
                            REACTIONS_FILE,
                            format!("reaction {:?}: pH {n} above {PH_MAX}", r.id),
                        ));
                    }
                    Condition::Tag(tag) => non_empty(REACTIONS_FILE, "where tag", tag)?,
                    _ => {}
                }
            }
            match &r.side {
                Some(SideEffect::Coat(_, crust, x, _)) => {
                    material_ref(crust);
                    key_in(REACTIONS_FILE, &r.id, "substance", x, &sub_keys)?;
                }
                Some(SideEffect::EmitGas(g, _)) => {
                    key_in(REACTIONS_FILE, &r.id, "gas", g, &gas_keys)?;
                }
                Some(SideEffect::WeatherBoost(_)) | None => {}
            }
        }

        unique_keys(PLANTS_FILE, "plant", plants.iter().map(|p| &p.key))?;
        for p in &plants {
            material_ref(&p.material);
            if p.anchor_tags.is_empty() {
                return Err(invalid(
                    PLANTS_FILE,
                    format!("plant {:?} has no anchor_tags", p.key),
                ));
            }
            for tag in &p.anchor_tags {
                non_empty(PLANTS_FILE, "anchor tag", tag)?;
            }
            if p.needs.temp.0 > p.needs.temp.1 {
                return Err(invalid(
                    PLANTS_FILE,
                    format!("plant {:?} temp range is empty", p.key),
                ));
            }
            pairs_in(
                PLANTS_FILE,
                &p.key,
                "substance_min",
                &p.needs.substance_min,
                &sub_keys,
            )?;
            pairs_in(
                PLANTS_FILE,
                &p.key,
                "substance_max",
                &p.needs.substance_max,
                &sub_keys,
            )?;
            pairs_in(PLANTS_FILE, &p.key, "uptake", &p.uptake, &sub_keys)?;
        }

        if seasons.substeps == 0 {
            return Err(invalid(SEASONS_FILE, "substeps is 0"));
        }
        if seasons.seasons.is_empty() {
            return Err(invalid(SEASONS_FILE, "no seasons"));
        }
        unique_keys(
            SEASONS_FILE,
            "season",
            seasons.seasons.iter().map(|s| &s.key),
        )?;

        validate_params(&params)?;

        for (kind, required, map) in [
            ("materials", &MATERIAL_ROLES[..], &roles.materials),
            ("gases", &GAS_ROLES[..], &roles.gases),
            ("substances", &SUBSTANCE_ROLES[..], &roles.substances),
        ] {
            for role in required {
                if !map.contains_key(*role) {
                    return Err(invalid(ECO_FILE, format!("roles.{kind} lacks {role:?}")));
                }
            }
        }
        for key in roles.materials.values() {
            material_ref(key);
        }
        for (role, key) in &roles.gases {
            key_in(ECO_FILE, role, "gas role", key, &gas_keys)?;
        }
        for (role, key) in &roles.substances {
            key_in(ECO_FILE, role, "substance role", key, &sub_keys)?;
        }
        unique_keys(ECO_FILE, "tint", tint.iter().map(|r| &r.substance))?;
        for row in &tint {
            key_in(ECO_FILE, "tint", "substance", &row.substance, &sub_keys)?;
            if row.full == 0 {
                return Err(invalid(
                    ECO_FILE,
                    format!("tint {:?} has full 0", row.substance),
                ));
            }
        }
        unique_keys(ECO_FILE, "gas_tint", gas_tint.iter().map(|r| &r.gas))?;
        for row in &gas_tint {
            key_in(ECO_FILE, "gas_tint", "gas", &row.gas, &gas_keys)?;
            if row.full == 0 {
                return Err(invalid(
                    ECO_FILE,
                    format!("gas_tint {:?} has full 0", row.gas),
                ));
            }
            // Only gases the player can see tint the view (§7.4 `visible`).
            if gases.iter().any(|g| g.key == row.gas && !g.visible) {
                return Err(invalid(
                    ECO_FILE,
                    format!("gas_tint {:?} names a gas that is not visible", row.gas),
                ));
            }
        }
        // Role `air` is the only Phase::Air material (SAD-P2 §3).
        let air_key = &roles.materials["air"];
        for d in materials.iter().filter(|d| d.phase == Phase::Air) {
            if &d.key != air_key {
                return Err(invalid(
                    ECO_FILE,
                    format!(
                        "material {:?} is Phase::Air but role air is {air_key:?}",
                        d.key
                    ),
                ));
            }
        }
        if unresolved.contains(air_key) {
            return Err(invalid(
                ECO_FILE,
                format!("role air names unknown {air_key:?}"),
            ));
        }

        if !unresolved.is_empty() {
            tracing::warn!(
                keys = ?unresolved,
                "eco tables name materials the material table does not have yet"
            );
        }

        let mut tables = Self {
            materials,
            substances,
            substance_layers,
            gases,
            transitions,
            reactions,
            plants,
            seasons,
            params,
            roles,
            eco_test,
            tint,
            gas_tint,
            unresolved,
            hash: 0,
        };
        tables.hash = tables.compute_hash();
        Ok(tables)
    }

    fn compute_hash(&self) -> u64 {
        let tables = (
            &self.substances,
            &self.gases,
            &self.transitions,
            &self.reactions,
            &self.plants,
            &self.seasons,
            &self.params,
            &self.roles,
            &self.eco_test,
        );
        // Tables without `tint` hash as they did before the section existed
        // (F146), and tables without `gas_tint` as they did before F154, so
        // the golden hash and older save headers still match.
        match (self.tint.is_empty(), self.gas_tint.is_empty()) {
            (true, true) => tables_hash(self.materials.hash(), &tables),
            (false, true) => tables_hash(self.materials.hash(), &(tables, &self.tint)),
            _ => tables_hash(self.materials.hash(), &(tables, &self.tint, &self.gas_tint)),
        }
    }

    /// These tables with every season row's `rain` and `air_temp` replaced
    /// by the override's given fields (SAD-P2 §11 `set_season_params`).
    /// The hash covers the result, so it moves with any change.
    pub fn with_season_override(&self, o: SeasonOverride) -> Self {
        let mut t = self.clone();
        for row in &mut t.seasons.seasons {
            if let Some(rain) = o.rain {
                row.rain = rain;
            }
            if let Some(air_temp) = o.air_temp {
                row.air_temp = air_temp;
            }
        }
        t.hash = t.compute_hash();
        t
    }

    /// xxh3 over the material table hash and the canonical (postcard)
    /// encoding of every Phase 2 table. Independent of RON formatting.
    pub fn hash(&self) -> u64 {
        self.hash
    }

    pub fn materials(&self) -> &Arc<MaterialTable> {
        &self.materials
    }

    pub fn substances(&self) -> &[SubstanceDef] {
        &self.substances
    }

    /// Substance keys to layers, by row order (SAD-P2 §7.6).
    pub fn substance_layers(&self) -> &SubstanceLayers {
        &self.substance_layers
    }

    pub fn gases(&self) -> &[GasDef] {
        &self.gases
    }

    pub fn transitions(&self) -> &[TransitionDef] {
        &self.transitions
    }

    pub fn reactions(&self) -> &[ReactionDef] {
        &self.reactions
    }

    pub fn plants(&self) -> &[PlantDef] {
        &self.plants
    }

    pub fn seasons(&self) -> &SeasonTable {
        &self.seasons
    }

    pub fn params(&self) -> &EcoParams {
        &self.params
    }

    pub fn roles(&self) -> &Roles {
        &self.roles
    }

    pub fn eco_test(&self) -> &EcoTestParams {
        &self.eco_test
    }

    /// The `tint` rows of `eco.ron`, in order.
    pub fn tint(&self) -> &[TintDef] {
        &self.tint
    }

    /// The `tint` rows resolved to substance layers (SAD-P2 §10), in order:
    /// what the client's liquid pass applies.
    pub fn substance_tints(&self) -> Vec<SubstanceTint> {
        self.tint
            .iter()
            .filter_map(|row| {
                Some(SubstanceTint {
                    layer: self.substance_layers.layer(&row.substance)?,
                    color: row.color,
                    full: row.full,
                    strength: row.strength,
                })
            })
            .collect()
    }

    /// The `gas_tint` rows of `eco.ron`, in order.
    pub fn gas_tint(&self) -> &[GasTintDef] {
        &self.gas_tint
    }

    /// The `gas_tint` rows resolved to gas layers (SAD-P2 §10), in order:
    /// what the client composites over the view at the camera's cell.
    pub fn gas_tints(&self) -> Vec<SubstanceTint> {
        self.gas_tint
            .iter()
            .filter_map(|row| {
                let i = self.gases.iter().position(|g| g.key == row.gas)?;
                Some(SubstanceTint {
                    layer: LayerId::Gas(u8::try_from(i).ok()?),
                    color: row.color,
                    full: row.full,
                    strength: row.strength,
                })
            })
            .collect()
    }

    /// The flammable gases' layers with their `ignite_min`, in row order:
    /// what the client's lantern icon reads (SAD-P2 §10).
    pub fn flammable_gases(&self) -> Vec<(LayerId, u8)> {
        self.gases
            .iter()
            .enumerate()
            .filter(|(_, g)| g.flammable)
            .filter_map(|(i, g)| Some((LayerId::Gas(u8::try_from(i).ok()?), g.ignite_min)))
            .collect()
    }

    /// Material keys the eco files name that the material table lacks.
    pub fn unresolved_materials(&self) -> &BTreeSet<String> {
        &self.unresolved
    }

    /// Layer index of a substance key.
    pub fn substance_index(&self, key: &str) -> Option<u8> {
        match self.substance_layers.layer(key)? {
            LayerId::Substance(i) => Some(i),
            _ => None,
        }
    }

    /// Layer index of a gas key.
    pub fn gas_index(&self, key: &str) -> Option<u8> {
        index_of(self.gases.iter().map(|g| &g.key), key)
    }

    /// Material a role names, if the material table has it yet.
    pub fn role_material(&self, role: &str) -> Option<MaterialId> {
        self.materials.id(self.roles.materials.get(role)?)
    }

    /// Gas index a role names.
    pub fn role_gas(&self, role: &str) -> Option<u8> {
        self.gas_index(self.roles.gases.get(role)?)
    }

    /// Substance index a role names.
    pub fn role_substance(&self, role: &str) -> Option<u8> {
        self.substance_index(self.roles.substances.get(role)?)
    }

    /// The toxicity profile of `kind` (SAD-P2 §7.6, §10): per substance
    /// row, its `tox_plant` or `tox_player`, absent meaning never toxic.
    pub fn tox_profile(&self, kind: ToxKind) -> ToxProfile {
        let mut limits = [None; N_SUBSTANCES];
        for (slot, s) in limits.iter_mut().zip(&self.substances) {
            *slot = match kind {
                ToxKind::Plant => s.tox_plant,
                ToxKind::Player => s.tox_player,
            };
        }
        ToxProfile::new(limits)
    }

    /// What pH reads (SAD-P2 §7.6): the `acid` and `lime` substance roles
    /// and `eco.ph_shift`.
    pub fn ph_rule(&self) -> PhRule {
        PhRule {
            acid: self.role_substance("acid").map(LayerId::Substance),
            lime: self.role_substance("lime").map(LayerId::Substance),
            ph_shift: self.params.ph_shift,
        }
    }

    /// The roles and constants `World::apply` and `World::eco_set` read for
    /// the displacement rules (SAD-P2 §7.0). Roles whose material the table
    /// lacks yet resolve to `None`, which leaves their rule inert.
    pub fn displacement_rules(&self) -> EcoRules {
        let mut substance_yields = [None; N_SUBSTANCES];
        for (slot, s) in substance_yields.iter_mut().zip(&self.substances) {
            *slot = s
                .yield_material
                .as_deref()
                .and_then(|key| self.materials.id(key));
        }
        EcoRules {
            water: self.role_material("water"),
            ice: self.role_material("ice"),
            magma: self.role_material("magma"),
            steam: self.role_gas("steam"),
            organic: self.role_substance("organic"),
            magma_init_temp: self.params.magma_init_temp,
            plant_litter: self.plants.iter().map(|p| p.litter).collect(),
            plant_materials: self
                .plants
                .iter()
                .map(|p| self.materials.id(&p.material))
                .collect(),
            harvest_shift: self.params.harvest_shift,
            substance_yields,
            soft_regolith: Some(self.params.soft_regolith),
        }
    }
}

fn index_of<'a>(mut keys: impl Iterator<Item = &'a String>, key: &str) -> Option<u8> {
    keys.position(|k| k == key)
        .and_then(|i| u8::try_from(i).ok())
}

fn non_empty(file: &'static str, what: &str, s: &str) -> Result<(), EcoTableError> {
    if s.is_empty() {
        return Err(invalid(file, format!("empty {what}")));
    }
    Ok(())
}

fn unique_keys<'a>(
    file: &'static str,
    what: &str,
    keys: impl Iterator<Item = &'a String>,
) -> Result<(), EcoTableError> {
    let mut seen = BTreeSet::new();
    for k in keys {
        non_empty(file, &format!("{what} key"), k)?;
        if !seen.insert(k) {
            return Err(invalid(file, format!("duplicate {what} key {k:?}")));
        }
    }
    Ok(())
}

fn key_in(
    file: &'static str,
    owner: &str,
    what: &str,
    key: &str,
    known: &BTreeSet<&str>,
) -> Result<(), EcoTableError> {
    if !known.contains(key) {
        return Err(invalid(
            file,
            format!("{owner:?} names unknown {what} {key:?}"),
        ));
    }
    Ok(())
}

/// A `(key, amount)` list whose keys are known and unique.
fn pairs_in(
    file: &'static str,
    owner: &str,
    field: &str,
    pairs: &[(String, u8)],
    known: &BTreeSet<&str>,
) -> Result<(), EcoTableError> {
    for (i, (k, _)) in pairs.iter().enumerate() {
        key_in(file, owner, field, k, known)?;
        if pairs[..i].iter().any(|(p, _)| p == k) {
            return Err(invalid(file, format!("{owner:?} {field} repeats {k:?}")));
        }
    }
    Ok(())
}

fn validate_params(p: &EcoParams) -> Result<(), EcoTableError> {
    let shifts = [
        ("heat_shift", p.heat_shift),
        ("infil_shift", p.infil_shift),
        ("infil_side_shift", p.infil_side_shift),
        ("seep_shift", p.seep_shift),
        ("drift_shift", p.drift_shift),
        ("diff_shift", p.diff_shift),
        ("ph_shift", p.ph_shift),
        ("harvest_shift", p.harvest_shift),
    ];
    for (name, v) in shifts {
        if v > MAX_SHIFT {
            return Err(invalid(ECO_FILE, format!("{name} {v} above {MAX_SHIFT}")));
        }
    }
    let positive = [
        ("flow_iters", u32::from(p.flow_iters)),
        ("quiet_skip", u32::from(p.quiet_skip)),
        ("evap_table length", p.evap_table.len() as u32),
    ];
    for (name, v) in positive {
        if v == 0 {
            return Err(invalid(ECO_FILE, format!("{name} is 0")));
        }
    }
    if p.evap_bucket <= 0 {
        return Err(invalid(ECO_FILE, "evap_bucket must be positive"));
    }
    if p.seep_num > SEEP_DEN {
        return Err(invalid(ECO_FILE, format!("seep_num above {SEEP_DEN}")));
    }
    if p.acid_ph > PH_MAX {
        return Err(invalid(ECO_FILE, format!("acid_ph above {PH_MAX}")));
    }
    Ok(())
}

fn tables_hash(materials_hash: u64, tables: &impl Serialize) -> u64 {
    let mut buf = b"rmc.eco.tables.v1".to_vec();
    buf.extend_from_slice(&materials_hash.to_le_bytes());
    buf.extend(postcard::to_stdvec(tables).expect("postcard encodes plain table data"));
    xxhash_rust::xxh3::xxh3_64(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    fn shipped() -> EcoTables {
        EcoTables::load(ASSETS).expect("Phase 2 tables load")
    }

    fn shipped_parts() -> (Arc<MaterialTable>, EcoSources) {
        let materials =
            Arc::new(MaterialTable::load(Path::new(ASSETS).join(MATERIALS_FILE)).unwrap());
        (materials, EcoSources::read(ASSETS).unwrap())
    }

    #[test]
    fn tox_profiles_and_ph_rule_follow_the_tables() {
        let t = shipped();
        let (plant, player) = (
            t.tox_profile(ToxKind::Plant),
            t.tox_profile(ToxKind::Player),
        );
        for (i, s) in (0u8..).zip(t.substances()) {
            let id = LayerId::Substance(i);
            assert_eq!(plant.limit(id), s.tox_plant, "{}", s.key);
            assert_eq!(player.limit(id), s.tox_player, "{}", s.key);
        }
        for i in t.substances().len()..N_SUBSTANCES {
            let id = LayerId::Substance(u8::try_from(i).unwrap());
            assert_eq!((plant.limit(id), player.limit(id)), (None, None));
        }
        assert!(!player.is_empty(), "the shipped rows protect the player");
        assert_eq!(
            player.limit(t.substance_layers().layer("cu").unwrap()),
            Some(80)
        );

        let rule = t.ph_rule();
        assert_eq!(rule.ph_shift, t.params().ph_shift);
        assert_eq!(rule.acid, t.substance_layers().layer("acid"));
        assert_eq!(rule.lime, t.substance_layers().layer("lime"));
    }

    #[test]
    fn gas_tint_rows_resolve_to_visible_gas_layers() {
        let t = shipped();
        let tints = t.gas_tints();
        assert!(!tints.is_empty(), "the shipped gas tints");
        assert_eq!(tints.len(), t.gas_tint().len());
        for (row, tint) in t.gas_tint().iter().zip(&tints) {
            let i = t.gases().iter().position(|g| g.key == row.gas).unwrap();
            assert_eq!(tint.layer, LayerId::Gas(u8::try_from(i).unwrap()));
            assert!(t.gases()[i].visible, "{} is visible", row.gas);
            assert_eq!(
                (tint.color, tint.full, tint.strength),
                (row.color, row.full, row.strength)
            );
            assert!(tint.full > 0 && tint.strength > 0, "{}", row.gas);
        }
        // Every flammable row, with its ignition threshold.
        let flammable = t.flammable_gases();
        let want: Vec<(LayerId, u8)> = t
            .gases()
            .iter()
            .enumerate()
            .filter(|(_, g)| g.flammable)
            .map(|(i, g)| (LayerId::Gas(u8::try_from(i).unwrap()), g.ignite_min))
            .collect();
        assert!(!want.is_empty());
        assert_eq!(flammable, want);

        // The hash covers the rows.
        let edited =
            load_edited(|src| replace_once(&mut src.eco, "full: 160", "full: 161")).unwrap();
        assert_ne!(edited.hash(), t.hash());
        // An invisible gas, an unknown gas and full 0 are refused.
        let invisible = t.gases().iter().find(|g| !g.visible).unwrap().key.clone();
        let first = t.gas_tint()[0].gas.clone();
        for (from, to) in [
            (format!("gas: {first:?}"), format!("gas: {invisible:?}")),
            (format!("gas: {first:?}"), "gas: \"nope\"".to_string()),
            ("full: 160".to_string(), "full: 0".to_string()),
        ] {
            assert!(
                matches!(
                    load_edited(|src| replace_once(&mut src.eco, &from, &to)),
                    Err(EcoTableError::Invalid { file: ECO_FILE, .. })
                ),
                "{to}"
            );
        }
    }

    #[test]
    fn tint_rows_resolve_to_substance_layers() {
        let t = shipped();
        let tints = t.substance_tints();
        assert!(!tints.is_empty(), "the shipped water tints");
        assert_eq!(tints.len(), t.tint().len());
        for (row, tint) in t.tint().iter().zip(&tints) {
            assert_eq!(t.substance_layers().layer(&row.substance), Some(tint.layer));
            assert_eq!(
                (tint.color, tint.full, tint.strength),
                (row.color, row.full, row.strength)
            );
            assert!(tint.full > 0 && tint.strength > 0, "{}", row.substance);
        }
        let fe = t.substance_layers().layer("fe").unwrap();
        assert!(tints.iter().any(|r| r.layer == fe), "iron tints the water");

        // The hash covers the rows; a file without them has no tint.
        let edited = load_edited(|src| replace_once(&mut src.eco, "full: 32", "full: 33")).unwrap();
        assert_ne!(edited.hash(), t.hash());
        let none = load_edited(|src| {
            let start = src.eco.find("    tint: [").expect("tint section");
            let end = start
                + src.eco[start..]
                    .find(
                        "],
",
                    )
                    .expect("tint end")
                + 3;
            src.eco.replace_range(start..end, "");
        })
        .unwrap();
        assert!(none.substance_tints().is_empty());

        // Rows must name a known substance once, with full above 0.
        for (from, to) in [
            ("substance: \"fe\"", "substance: \"rust\""),
            ("substance: \"cu\"", "substance: \"fe\""),
            ("full: 32", "full: 0"),
        ] {
            let err = load_edited(|src| replace_once(&mut src.eco, from, to));
            assert!(
                matches!(err, Err(EcoTableError::Invalid { file: ECO_FILE, .. })),
                "{to}: {err:?}"
            );
        }
    }

    #[test]
    fn season_override_replaces_given_fields_of_every_row() {
        let t = shipped();
        assert_eq!(t.with_season_override(SeasonOverride::default()), t);

        let rain = t.with_season_override(SeasonOverride {
            rain: Some(0),
            air_temp: None,
        });
        assert_ne!(rain.hash(), t.hash(), "the hash covers the seasons");
        assert_eq!(rain.seasons().substeps, t.seasons().substeps);
        assert_eq!(rain.seasons().seasons.len(), t.seasons().seasons.len());
        for (a, b) in rain.seasons().seasons.iter().zip(&t.seasons().seasons) {
            assert_eq!((&a.key, a.air_temp, a.rain), (&b.key, b.air_temp, 0));
        }

        let both = rain.with_season_override(SeasonOverride {
            rain: Some(200),
            air_temp: Some(-150),
        });
        assert!(
            both.seasons()
                .seasons
                .iter()
                .all(|s| (s.rain, s.air_temp) == (200, -150))
        );
        assert_eq!(both.params(), t.params(), "only the seasons change");
    }

    #[test]
    fn displacement_rules_come_from_roles_and_params() {
        let t = shipped();
        let rules = t.displacement_rules();
        assert_eq!(rules.water, t.role_material("water"));
        assert_eq!(rules.ice, t.role_material("ice"));
        assert_eq!(rules.magma, t.role_material("magma"));
        assert_eq!(rules.steam, t.role_gas("steam"));
        assert!(rules.steam.is_some(), "steam role resolves");
        assert_eq!(rules.organic, t.role_substance("organic"));
        assert!(rules.organic.is_some(), "organic role resolves");
        assert_eq!(rules.magma_init_temp, t.params().magma_init_temp);
        assert_eq!(rules.harvest_shift, t.params().harvest_shift);
        let litter: Vec<u8> = t.plants().iter().map(|p| p.litter).collect();
        assert_eq!(rules.plant_litter, litter);
        let plants: Vec<_> = t
            .plants()
            .iter()
            .map(|p| t.materials().id(&p.material))
            .collect();
        assert!(
            plants.iter().all(Option::is_some),
            "every species' material exists"
        );
        assert_eq!(rules.plant_materials, plants);
        for (i, s) in t.substances().iter().enumerate() {
            let want = s
                .yield_material
                .as_deref()
                .and_then(|k| t.materials().id(k));
            assert_eq!(rules.substance_yields[i], want, "{}", s.key);
        }
        assert!(
            rules.substance_yields[t.substances().len()..]
                .iter()
                .all(Option::is_none)
        );
    }

    /// Applies `edit` to the shipped sources and loads the result.
    fn load_edited(edit: impl FnOnce(&mut EcoSources)) -> Result<EcoTables, EcoTableError> {
        let (materials, mut src) = shipped_parts();
        edit(&mut src);
        EcoTables::from_sources(materials, &src)
    }

    fn replace_once(text: &mut String, from: &str, to: &str) {
        assert!(text.contains(from), "fixture text lacks {from:?}");
        *text = text.replacen(from, to, 1);
    }

    /// Materials SAD-P2 §5 adds in later features. The eco files may name
    /// them before the material table has them, and nothing else.
    const LATER_MATERIALS: [&str; 17] = [
        "water",
        "ice",
        "magma",
        "magma_source",
        "obsidian",
        "limestone",
        "sulfide_ore",
        "crushed_sulfide",
        "ochre_crust",
        "salt_crust",
        "moss",
        "grass",
        "fireweed",
        "shrub",
        "copper_dust",
        "iron_dust",
        "salt_dust",
    ];

    #[test]
    fn loads_all_phase2_tables() {
        let t = shipped();

        assert_eq!(
            t.substances()
                .iter()
                .map(|s| s.key.as_str())
                .collect::<Vec<_>>(),
            ["cu", "fe", "s", "acid", "lime", "salt", "n", "p", "k", "c"]
        );
        assert_eq!(t.substances().len(), N_SUBSTANCES);
        assert_eq!(t.substance_index("cu"), Some(0));
        assert_eq!(t.substance_index("c"), Some(9));
        assert_eq!(t.substance_index("gold"), None);
        let yields: Vec<(&str, &str)> = t
            .substances()
            .iter()
            .filter_map(|s| Some((s.key.as_str(), s.yield_material.as_deref()?)))
            .collect();
        assert_eq!(
            yields,
            [
                ("cu", "copper_dust"),
                ("fe", "iron_dust"),
                ("salt", "salt_dust")
            ]
        );

        assert_eq!(
            t.gases().iter().map(|g| g.key.as_str()).collect::<Vec<_>>(),
            ["steam", "firedamp", "chokedamp", "smoke"]
        );
        assert_eq!(t.gases().len(), N_GASES);
        let gas = |k: &str| &t.gases()[usize::from(t.gas_index(k).unwrap())];
        assert!(gas("steam").density < 0, "steam rises");
        assert!(gas("firedamp").density < 0, "firedamp rises");
        assert!(gas("chokedamp").density > 0, "chokedamp sinks");
        assert!(gas("firedamp").flammable && gas("firedamp").ignite_min > 0);
        assert!(!gas("steam").flammable && !gas("chokedamp").flammable);

        // §7.2, verbatim.
        use MaterialMatch::{Key, Tag};
        use TransitionWhen::{TempAbove, TempBelow};
        let row = |from, to: &str, when, effect| TransitionDef {
            from,
            to: to.into(),
            when,
            effect,
        };
        assert_eq!(
            t.transitions(),
            [
                row(Key("water".into()), "ice", TempBelow(0), None),
                row(Key("ice".into()), "water", TempAbove(50), None),
                row(
                    Key("water".into()),
                    "air",
                    TempAbove(1000),
                    Some(TransitionEffect::BoilAll)
                ),
                row(Key("magma".into()), "basalt", TempBelow(9000), None),
                row(Tag("stone".into()), "magma", TempAbove(12500), None),
            ]
        );

        // §7.6, verbatim.
        let pairs = |v: &[(&str, u8)]| -> Vec<(String, u8)> {
            v.iter().map(|&(k, n)| (k.to_string(), n)).collect()
        };
        let reaction =
            |id: &str, consume: &[(&str, u8)], conditions, produce: &[(&str, u8)], side| {
                ReactionDef {
                    id: id.into(),
                    consume: pairs(consume),
                    conditions,
                    produce: pairs(produce),
                    side,
                }
            };
        assert_eq!(
            t.reactions(),
            [
                reaction(
                    "oxidize",
                    &[("s", 4)],
                    vec![Condition::Exposed, Condition::Tag("sulfide".into())],
                    &[("acid", 4)],
                    None
                ),
                reaction(
                    "neutralize",
                    &[("acid", 2), ("lime", 2)],
                    vec![],
                    &[("salt", 2)],
                    None
                ),
                reaction(
                    "dissolve_carbonate",
                    &[("acid", 1)],
                    vec![Condition::PhBelow(4), Condition::Tag("carbonate".into())],
                    &[("lime", 2)],
                    Some(SideEffect::WeatherBoost(16))
                ),
                reaction(
                    "ochre",
                    &[("fe", 8)],
                    vec![Condition::Submerged, Condition::PhAbove(5)],
                    &[],
                    Some(SideEffect::Coat(
                        CoatTarget::Floor,
                        "ochre_crust".into(),
                        "fe".into(),
                        8
                    ))
                ),
                reaction(
                    "salt_crust",
                    &[("salt", 16)],
                    vec![Condition::Evaporating],
                    &[],
                    Some(SideEffect::Coat(
                        CoatTarget::Floor,
                        "salt_crust".into(),
                        "salt".into(),
                        16
                    ))
                ),
                reaction(
                    "decompose",
                    &[("c", 4)],
                    vec![Condition::Tag("soil".into())],
                    &[("n", 1)],
                    Some(SideEffect::EmitGas("chokedamp".into(), 2))
                ),
            ]
        );

        // §7.8: four pioneer species.
        assert_eq!(
            t.plants()
                .iter()
                .map(|p| p.key.as_str())
                .collect::<Vec<_>>(),
            ["moss", "grass", "fireweed", "shrub"]
        );
        let plant = |k: &str| t.plants().iter().find(|p| p.key == k).unwrap();
        assert!(plant("fireweed").holds, "fireweed hyperaccumulates");
        assert!(
            plant("fireweed")
                .uptake
                .iter()
                .any(|(x, n)| x == "cu" && *n > 0)
        );
        let cu_max = |k: &str| {
            plant(k)
                .needs
                .substance_max
                .iter()
                .find(|(x, _)| x == "cu")
                .map_or(u8::MAX, |&(_, n)| n)
        };
        assert!(
            cu_max("grass") < cu_max("fireweed"),
            "grass dies on copper soil"
        );
        assert!(plant("shrub").tall);
        for p in t.plants() {
            assert!(
                p.lifespan_seasons > 0 && p.growth_per_substep > 0,
                "{}",
                p.key
            );
        }

        // §3: 8 sub-steps; the cycle crosses 0 °C so freeze-thaw can happen.
        assert_eq!(t.seasons().substeps, 8);
        assert!(t.seasons().seasons.iter().any(|s| s.air_temp < 0));
        assert!(t.seasons().seasons.iter().any(|s| s.air_temp > 0));

        // Named constants with SAD-given values.
        let p = t.params();
        assert_eq!(p.heat_shift, 7);
        assert_eq!(p.flow_iters, 8);
        assert_eq!(p.magma_init_temp, 12000);
        assert_eq!(p.condense_temp, 900);
        assert_eq!(p.light_lateral_iters, 6);
        assert_eq!(t.eco_test().quiet_tick_dev_ms, 60000);

        // Roles.
        assert_eq!(t.role_material("air"), Some(MaterialId::AIR));
        assert_eq!(t.role_material("basalt"), t.materials().id("basalt"));
        assert!(t.role_material("basalt").is_some());
        assert_eq!(t.role_gas("steam"), t.gas_index("steam"));
        assert_eq!(t.role_gas("smoke"), t.gas_index("smoke"));
        assert_eq!(t.role_substance("organic"), t.substance_index("c"));
        assert_eq!(t.role_substance("acid"), t.substance_index("acid"));
        assert_eq!(t.role_substance("lime"), t.substance_index("lime"));
        assert_eq!(t.role_material("no_such_role"), None);
        for role in MATERIAL_ROLES {
            assert!(t.roles().materials.contains_key(role), "{role}");
        }

        // Unresolved material keys are exactly later-feature materials.
        for k in t.unresolved_materials() {
            assert!(
                LATER_MATERIALS.contains(&k.as_str()),
                "unexpected material {k:?}"
            );
        }
        assert!(!t.unresolved_materials().contains("air"));
        assert!(!t.unresolved_materials().contains("basalt"));

        // The material table is the shipped one.
        let (materials, _) = shipped_parts();
        assert_eq!(t.materials().hash(), materials.hash());

        // Rejections.
        let invalid_in = |r: Result<EcoTables, EcoTableError>, want: &str| match r {
            Err(EcoTableError::Invalid { file, .. }) => assert_eq!(file, want),
            other => panic!("expected Invalid in {want}, got {other:?}"),
        };
        invalid_in(
            load_edited(|s| replace_once(&mut s.substances, "key: \"fe\"", "key: \"cu\"")),
            SUBSTANCES_FILE,
        );
        invalid_in(
            load_edited(|s| replace_once(&mut s.reactions, "(\"s\", 4)", "(\"gold\", 4)")),
            REACTIONS_FILE,
        );
        invalid_in(
            load_edited(|s| {
                replace_once(
                    &mut s.reactions,
                    "EmitGas(\"chokedamp\"",
                    "EmitGas(\"ether\"",
                )
            }),
            REACTIONS_FILE,
        );
        invalid_in(
            load_edited(|s| replace_once(&mut s.reactions, "PhBelow(4)", "PhBelow(15)")),
            REACTIONS_FILE,
        );
        invalid_in(
            load_edited(|s| replace_once(&mut s.plants, "(\"cu\", 2)", "(\"gold\", 2)")),
            PLANTS_FILE,
        );
        invalid_in(
            load_edited(|s| replace_once(&mut s.eco, r#""organic": "c""#, r#""organic": "wood""#)),
            ECO_FILE,
        );
        invalid_in(
            load_edited(|s| replace_once(&mut s.eco, r#""steam": "steam""#, r#""vapor": "steam""#)),
            ECO_FILE,
        );
        invalid_in(
            load_edited(|s| replace_once(&mut s.eco, "heat_shift: 7", "heat_shift: 31")),
            ECO_FILE,
        );
        invalid_in(
            load_edited(|s| replace_once(&mut s.seasons, "substeps: 8", "substeps: 0")),
            SEASONS_FILE,
        );
        invalid_in(
            load_edited(|s| {
                let fifth = "(key: \"ether\", density: 0, diffusion: 1, flammable: false, \
                             ignite_min: 0, ignite_radius_per_64: 0, visible: false),\n]";
                let close = s.gases.rfind(']').unwrap();
                s.gases.replace_range(close.., fifth);
            }),
            GASES_FILE,
        );
        // A material whose composition names an unknown substance.
        let (materials, src) = shipped_parts();
        let mut defs: Vec<_> = materials.iter().cloned().collect();
        defs[4].composition = vec![("gold".into(), 1)];
        let bad = Arc::new(MaterialTable::from_defs(defs).unwrap());
        invalid_in(EcoTables::from_sources(bad, &src), MATERIALS_FILE);
        // Unknown fields are parse errors, not silently ignored.
        assert!(matches!(
            load_edited(|s| replace_once(&mut s.eco, "flow_cap:", "flow_cpa:")),
            Err(EcoTableError::Parse { file: ECO_FILE, .. })
        ));
        assert!(matches!(
            EcoTables::load("does/not/exist"),
            Err(EcoTableError::Materials(_))
        ));
    }

    /// Small fixed tables, independent of the shipped assets so tuning them
    /// does not move the golden hash.
    fn fixture() -> (Arc<MaterialTable>, EcoSources) {
        let materials = MaterialTable::from_ron_str(
            r#"[
            (id: 0, key: "air", phase: Air, tags: [], hardness: 0, diggable: false,
             yield_count: 0, color: (0, 0, 0, 0), emissive: 0.0),
            (id: 1, key: "rock", phase: Solid, tags: ["stone"], hardness: 30, diggable: true,
             yield_count: 1, color: (128, 128, 128, 255), emissive: 0.0,
             composition: [("x", 10)]),
        ]"#,
        )
        .unwrap();
        let src = EcoSources {
            substances: r#"#![enable(implicit_some)]
                [(key: "x", mobility: 1, mobility_acid: 2, plant_nutrient: false, tox_plant: 3)]"#
                .into(),
            gases: r#"[(key: "g", density: -1, diffusion: 4, flammable: true, ignite_min: 5,
                ignite_radius_per_64: 1, visible: true)]"#
                .into(),
            transitions: r#"[(from: Tag("stone"), to: "air", when: TempAbove(100))]"#.into(),
            reactions: r#"#![enable(implicit_some)]
                [(id: "r", consume: [("x", 1)], where: [Exposed], side: EmitGas("g", 1))]"#
                .into(),
            plants: r#"[(key: "p", material: "rock", anchor_tags: ["stone"],
                needs: (light: 1, moisture: 2, temp: (0, 10)), growth_per_substep: 1,
                spread_chance: 2, spread_radius: 1, litter: 3, lifespan_seasons: 4)]"#
                .into(),
            seasons: r#"(substeps: 2, seasons: [(key: "s", air_temp: -5, rain: 9)])"#.into(),
            eco: r#"(
                params: (heat_shift: 7, geothermal_base: 400, gradient: 5,
                    magma_init_temp: 12000, flow_iters: 8, flow_cap: 64, infil_shift: 2,
                    infil_side_shift: 3, seep_num: 192, seep_shift: 3, condense_temp: 900,
                    evap_table: [1, 2], evap_bucket: 100, drift_shift: 2, diff_shift: 6,
                    soft_regolith: 128, ph_shift: 4, acid_ph: 5, evap_low_fill: 32,
                    crust_threshold: 64, light_lateral_iters: 6, light_falloff: 32,
                    stress_death: 16, harvest_shift: 3, quiet_skip: 2),
                roles: (
                    materials: {"air": "air", "water": "w", "ice": "i", "magma": "m",
                        "obsidian": "o", "basalt": "rock"},
                    gases: {"steam": "g", "smoke": "g", "firedamp": "g", "chokedamp": "g"},
                    substances: {"organic": "x", "acid": "x", "lime": "x"},
                ),
                eco_test: (swale_min: 8, quiet_tick_dev_ms: 60000),
            )"#
            .into(),
        };
        (Arc::new(materials), src)
    }

    #[test]
    fn tables_hash_is_stable() {
        let (m, src) = fixture();
        let a = EcoTables::from_sources(m.clone(), &src).unwrap();
        let b = EcoTables::from_sources(m.clone(), &src).unwrap();
        assert_eq!(a.hash(), b.hash());
        assert_eq!(
            a.unresolved_materials().iter().collect::<Vec<_>>(),
            ["i", "m", "o", "w"]
        );

        // Golden value: the canonical encoding must not drift between builds,
        // machines or crate upgrades, because the save header carries it.
        assert_eq!(a.hash(), 0x2ad8_64b6_1db1_7aef, "golden hash moved");

        // Formatting and comments do not matter.
        let mut spaced = src.clone();
        spaced.eco = format!("// constants\n{}\n", src.eco.replace(", ", ",\n  "));
        spaced.plants = src.plants.replace('\n', " ");
        let c = EcoTables::from_sources(m.clone(), &spaced).unwrap();
        assert_eq!(c.hash(), a.hash());

        // Every file and the material table contribute.
        let edits: [fn(&mut EcoSources); 8] = [
            |s| replace_once(&mut s.substances, "mobility: 1", "mobility: 9"),
            |s| replace_once(&mut s.gases, "diffusion: 4", "diffusion: 5"),
            |s| replace_once(&mut s.transitions, "TempAbove(100)", "TempAbove(101)"),
            |s| replace_once(&mut s.reactions, "[Exposed]", "[Submerged]"),
            |s| replace_once(&mut s.plants, "litter: 3", "litter: 4"),
            |s| replace_once(&mut s.seasons, "rain: 9", "rain: 8"),
            |s| replace_once(&mut s.eco, "light_falloff: 32", "light_falloff: 31"),
            |s| {
                replace_once(
                    &mut s.eco,
                    "\"lime\": \"x\"",
                    "\"lime\": \"x\", \"salt\": \"x\"",
                )
            },
        ];
        for (i, edit) in edits.iter().enumerate() {
            let mut s = src.clone();
            edit(&mut s);
            let changed = EcoTables::from_sources(m.clone(), &s).unwrap();
            assert_ne!(changed.hash(), a.hash(), "edit {i} did not change the hash");
        }
        let mut defs: Vec<_> = m.iter().cloned().collect();
        defs[1].hardness += 1;
        let m2 = Arc::new(MaterialTable::from_defs(defs).unwrap());
        assert_ne!(EcoTables::from_sources(m2, &src).unwrap().hash(), a.hash());

        // The shipped tables hash the same on every load.
        assert_eq!(shipped().hash(), shipped().hash());
        assert_ne!(shipped().hash(), a.hash());
    }
}
