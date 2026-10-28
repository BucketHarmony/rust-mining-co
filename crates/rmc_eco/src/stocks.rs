//! Vegetation stocks (docs/VEGETATION.md V7): what a world's plants hold,
//! measured after a season for the headless report.
//!
//! [`VegetationStocks::measure`] counts plants per species and part cells
//! per kind, the ground cover (columns holding a plant or a part), the
//! roots' stored biomass, the plant nutrients in anchor cells, and per
//! plant the factor that holds it back: the first of its needs that fails
//! (`moisture`, `light`, `temp`, `n >= 4`, `cu <= 24`, ...), else its
//! limiting demanded substance (V2 [`sufficiency`]) when short, else
//! `fed`. Every map is keyed and sorted, so the report is deterministic.

use std::collections::{BTreeMap, BTreeSet};

use rmc_world::coords::{CellPos, LocalPos};
use rmc_world::layers::{LayerData, LayerId, Part};
use rmc_world::query::{SUFFICIENT, sufficiency};
use rmc_world::world::World;
use serde::{Deserialize, Serialize};

use crate::tables::EcoTables;

/// The limiting-factor key of a plant that lacks nothing.
pub const FED: &str = "fed";

/// A world's vegetation (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VegetationStocks {
    /// Plants per species key.
    pub plants: BTreeMap<String, u32>,
    /// Part cells of multi-cell plants per kind (`Trunk`, `Leaf`, ...).
    pub parts: BTreeMap<String, u32>,
    /// Columns (x, z) holding a plant or a part.
    pub cover: u32,
    /// The roots' [`LayerId::Biomass`], summed.
    pub biomass: i64,
    /// Per plant-nutrient key (any substance a species needs, demands or
    /// takes up): the amount held by anchor cells.
    pub soil: BTreeMap<String, i64>,
    /// Plants per limiting factor (see the module docs).
    pub limits: BTreeMap<String, u32>,
}

impl VegetationStocks {
    /// Measures `world`'s resident chunks.
    pub fn measure(world: &World, t: &EcoTables) -> Self {
        let mut s = Self::default();
        let m = t.materials();
        let looks = t.plant_looks();
        let mut columns = BTreeSet::new();
        let nutrients: BTreeSet<u8> = t
            .plants()
            .iter()
            .flat_map(|p| {
                p.needs
                    .substance_min
                    .iter()
                    .chain(&p.demand)
                    .chain(&p.uptake)
                    .map(|(k, _)| k)
            })
            .filter_map(|k| t.substance_index(k))
            .collect();
        for (pos, chunk) in world.iter_chunks_sorted() {
            // Chunk-local indices fit in u16 (CHUNK_VOLUME cells).
            let local = |i: usize| LocalPos::from_index(i as u16);
            let at = |i: usize| CellPos::from_chunk_local(pos, local(i));
            for (&i, p) in &chunk.plants {
                let root = at(usize::from(i));
                columns.insert((root.0.x, root.0.z));
                s.biomass += i64::from(world.layer(root, LayerId::Biomass));
                let Some(def) = t.plants().get(usize::from(p.species)) else {
                    continue;
                };
                *s.plants.entry(def.key.clone()).or_default() += 1;
                let needs = looks.get(usize::from(p.species)).map(|l| &l.needs[..]);
                let limit = limit(world, t, def, needs.unwrap_or(&[]), root);
                *s.limits.entry(limit).or_default() += 1;
            }
            if let Some(LayerData::U8(v)) = chunk.layers.data(LayerId::Part) {
                for (i, &value) in v.iter().enumerate() {
                    if let Some(part) = Part::from_value(i32::from(value)) {
                        *s.parts.entry(format!("{:?}", part.kind)).or_default() += 1;
                        let c = at(i);
                        columns.insert((c.0.x, c.0.z));
                    }
                }
            }
            for &x in &nutrients {
                let Some(LayerData::U8(v)) = chunk.layers.data(LayerId::Substance(x)) else {
                    continue;
                };
                let held: i64 = v
                    .iter()
                    .enumerate()
                    .filter(|&(i, &n)| {
                        n > 0 && m.get(chunk.storage.get(local(i))).is_some_and(|d| d.anchor)
                    })
                    .map(|(_, &n)| i64::from(n))
                    .sum();
                let key = &t.substances()[usize::from(x)].key;
                *s.soil.entry(key.clone()).or_default() += held;
            }
        }
        for &x in &nutrients {
            s.soil
                .entry(t.substances()[usize::from(x)].key.clone())
                .or_default();
        }
        s.cover = u32::try_from(columns.len()).unwrap_or(u32::MAX);
        s
    }

    /// All plants.
    pub fn plant_count(&self) -> u32 {
        self.plants.values().sum()
    }
}

/// The factor holding back the plant of row `def` at `root` (see the module
/// docs); `needs` is its [`sufficiency`] demand.
fn limit(
    world: &World,
    t: &EcoTables,
    def: &crate::tables::PlantDef,
    needs: &[(u8, i32)],
    root: CellPos,
) -> String {
    let anchor = CellPos::new(root.0.x, root.0.y - 1, root.0.z);
    let sub = |k: &str| {
        t.substance_index(k)
            .map_or(0, |x| world.layer(anchor, LayerId::Substance(x)))
    };
    if world.layer(anchor, LayerId::Moisture) < i32::from(def.needs.moisture) {
        return "moisture".into();
    }
    if world.layer(root, LayerId::Light) < i32::from(def.needs.light) {
        return "light".into();
    }
    let temp = world.layer(root, LayerId::Temperature);
    if !(i32::from(def.needs.temp.0)..=i32::from(def.needs.temp.1)).contains(&temp) {
        return "temp".into();
    }
    if let Some((k, n)) = def
        .needs
        .substance_min
        .iter()
        .find(|(k, n)| sub(k) < i32::from(*n))
    {
        return format!("{k} >= {n}");
    }
    if let Some((k, n)) = def
        .needs
        .substance_max
        .iter()
        .find(|(k, n)| sub(k) > i32::from(*n))
    {
        return format!("{k} <= {n}");
    }
    let (s, short) = sufficiency(needs, |x| world.layer(anchor, LayerId::Substance(x)));
    match short.and_then(|i| needs.get(i)) {
        Some(&(x, _)) if s < SUFFICIENT => t.substances()[usize::from(x)].key.clone(),
        _ => FED.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, ChunkPos};
    use rmc_world::layers::{PartKind, PlantState};
    use rmc_world::materials::MaterialId;

    use crate::ledger::SeasonLedger;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    /// One chunk of loam up to y 10, air above.
    fn world(t: &EcoTables) -> World {
        let loam = t.materials().id("loam").unwrap();
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
        let mut w = World::new(Arc::clone(t.materials()), bounds);
        let mut chunk = Chunk::uniform(MaterialId::AIR);
        for z in 0..32 {
            for x in 0..32 {
                for y in 0..=10 {
                    chunk.storage.set(LocalPos::new(x, y, z), loam);
                }
            }
        }
        w.insert_chunk(ChunkPos::new(0, 0, 0), chunk).unwrap();
        w
    }

    /// A plant of `key` at `(x, 11, z)`, lit and warm, on an anchor holding
    /// `moisture` and `subs`.
    fn plant(
        w: &mut World,
        t: &EcoTables,
        key: &str,
        (x, z): (i32, i32),
        moisture: i32,
        subs: &[(&str, i32)],
    ) -> CellPos {
        let cell = CellPos::new(x, 11, z);
        let species = t.plants().iter().position(|p| p.key == key).unwrap();
        let def = &t.plants()[species];
        let mut scratch = SeasonLedger::default();
        w.eco_set(cell, t.materials().id(&def.material).unwrap(), &mut scratch);
        w.eco_put_plant(
            cell,
            Some(PlantState {
                species: species as u8,
                ..PlantState::default()
            }),
        );
        w.eco_put_layer(cell, LayerId::Light, 255);
        w.eco_put_layer(cell, LayerId::Temperature, 200);
        let anchor = CellPos::new(x, 10, z);
        w.eco_put_layer(anchor, LayerId::Moisture, moisture);
        for &(k, n) in subs {
            let s = t.substance_index(k).unwrap();
            w.eco_put_layer(anchor, LayerId::Substance(s), n);
        }
        cell
    }

    #[test]
    fn stocks_count_plants_parts_cover_soil_and_limits() {
        let t = EcoTables::load(ASSETS).unwrap();
        let mut w = world(&t);
        let empty = VegetationStocks::measure(&w, &t);
        assert_eq!(empty.plant_count(), 0);
        assert_eq!((empty.cover, empty.biomass), (0, 0));
        assert!(empty.soil.values().all(|&n| n == 0));
        assert!(["n", "p", "k"].iter().all(|k| empty.soil.contains_key(*k)));

        // Grass on fed, moist loam lacks nothing; clover short of
        // phosphorus is limited by it; moss on dry loam by moisture.
        plant(&mut w, &t, "grass", (2, 2), 100, &[("n", 50)]);
        plant(
            &mut w,
            &t,
            "clover",
            (4, 4),
            100,
            &[("n", 50), ("p", 1), ("k", 50)],
        );
        plant(&mut w, &t, "moss", (6, 6), 0, &[]);
        // An oak with a trunk part above its root and a leaf beside that.
        let root = plant(&mut w, &t, "oak", (10, 10), 100, &[]);
        w.eco_put_layer(root, LayerId::Biomass, 30);
        let trunk = CellPos::new(10, 12, 10);
        let leaf = CellPos::new(11, 12, 10);
        for (c, parent, kind) in [
            (trunk, root, PartKind::Trunk),
            (leaf, trunk, PartKind::Leaf),
        ] {
            let part = Part::toward(c, parent, kind).unwrap();
            w.eco_put_layer(c, LayerId::Part, i32::from(part.value()));
        }

        let s = VegetationStocks::measure(&w, &t);
        let one = |k: &str| (k.to_string(), 1);
        assert_eq!(
            s.plants,
            BTreeMap::from([one("clover"), one("grass"), one("moss"), one("oak")])
        );
        assert_eq!(s.parts, BTreeMap::from([one("Leaf"), one("Trunk")]));
        assert_eq!(s.cover, 5, "four plant columns and the leaf's");
        assert_eq!(s.biomass, 30);
        assert_eq!((s.soil["n"], s.soil["p"], s.soil["k"]), (100, 1, 50));
        assert_eq!(s.limits.get(FED), Some(&1), "{:?}", s.limits);
        assert_eq!(s.limits.get("p"), Some(&1), "{:?}", s.limits);
        assert_eq!(s.limits.get("moisture"), Some(&1), "{:?}", s.limits);
        assert_eq!(s.limits.values().sum::<u32>(), s.plant_count());

        // Substance in a cell that anchors nothing is no soil.
        let n = t.substance_index("n").unwrap();
        w.eco_put_layer(CellPos::new(20, 20, 20), LayerId::Substance(n), 40);
        assert_eq!(VegetationStocks::measure(&w, &t).soil["n"], 100);
    }

    /// A plot of 8 by 8 columns of fed, moist loam on `world`'s surface,
    /// holding `n` nitrogen and `c` organic matter per anchor, sown with
    /// `key` (or fallow), run through a year; returns the nitrogen in all
    /// the loam before and after.
    fn year_of(t: &EcoTables, key: Option<&str>, n: i32, c: i32) -> (i64, i64) {
        let mut w = world(t);
        w.set_eco_rules(t.displacement_rules());
        let loam = t.materials().id("loam").unwrap();
        let x = |k: &str| t.substance_index(k).unwrap();
        for z in 12..20 {
            for x0 in 12..20 {
                let anchor = CellPos::new(x0, 10, z);
                w.eco_put_layer(anchor, LayerId::Moisture, 100);
                w.eco_put_layer(anchor, LayerId::Substance(x("n")), n);
                w.eco_put_layer(anchor, LayerId::Substance(x("p")), 60);
                w.eco_put_layer(anchor, LayerId::Substance(x("k")), 60);
                w.eco_put_layer(anchor, LayerId::Substance(x("c")), c);
                if let Some(key) = key {
                    plant(&mut w, t, key, (x0, z), 100, &[]);
                }
            }
        }
        let nitrogen = |w: &World| -> i64 {
            let mut sum = 0;
            for (pos, chunk) in w.iter_chunks_sorted() {
                for i in 0..rmc_world::coords::CHUNK_VOLUME {
                    let local = LocalPos::from_index(i as u16);
                    if chunk.storage.get(local) == loam {
                        let cell = CellPos::from_chunk_local(pos, local);
                        sum += i64::from(w.layer(cell, LayerId::Substance(x("n"))));
                    }
                }
            }
            sum
        };
        let before = nitrogen(&w);
        crate::runner::activate_holders(&mut w, &crate::water::WaterPass::new(t));
        let mut activity = crate::activity::Activity::new(t.params().quiet_skip);
        for season in 0..4 {
            crate::tick::season_tick(
                &mut w,
                t,
                season,
                7,
                &mut activity,
                crate::runner::Exec::Serial,
            );
        }
        (before, nitrogen(&w))
    }

    #[test]
    fn soil_runs_down_under_grass_and_recovers_under_clover_and_fallow() {
        let t = EcoTables::load(ASSETS).unwrap();
        // Grass takes nitrogen up and holds none of it back.
        let (before, after) = year_of(&t, Some("grass"), 40, 0);
        assert!(after < before, "grass: {before} -> {after}");
        // Clover fixes it from the air on lean soil.
        let (before, after) = year_of(&t, Some("clover"), 4, 0);
        assert!(after > before, "clover: {before} -> {after}");
        // Fallow soil rich in litter gains it as the litter decomposes.
        let (before, after) = year_of(&t, None, 4, 120);
        assert!(after > before, "fallow: {before} -> {after}");
    }
}
