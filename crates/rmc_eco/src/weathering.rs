//! Weathering (SAD-P2 §7.5).
//!
//! **Freeze-thaw bit.** `Chunk.freeze_thaw` is cleared at the start of each
//! season ([`season_tick`]). In each sub-step the temperature pass sets a
//! cell's bit when [`crossed_zero`] holds for the cell's temperature before
//! the pass (before the clamps) and after it. The bit stays set for the
//! rest of the season; the weathering pass doubles its rate where it is
//! set.
//!
//! **Regolith growth.** A solid cell with `weathering_rate > 0` weathers
//! when it is exposed (an air or liquid face neighbor) and moist (its own
//! `moisture > 0`, or a water face neighbor):
//! `Δ = min(weathering_rate · (1 + freeze_thaw), 255 − regolith)` is added
//! to its `regolith`. The pass reads only the cell and its face neighbors
//! in the previous state, so it is a Jacobi pass (SAD-P2 §3).
//!
//! **Release.** With the growth, each `(x, total)` of the material's
//! `composition` releases `rel = total · Δ / 256` (truncating) of
//! substance `x` onto the cell: `x += min(rel, 255 − x)`. The ledger books
//! the release as `weather_in` and the part that finds no room as
//! `saturation_loss`, so `weather_in − saturation_loss` is what the cell
//! gained (the substance invariant, SAD-P2 §7).
//!
//! **Conversion.** A weathering cell whose regolith reaches 255 (with this
//! pass's growth, or already there) becomes its `weathers_to` material
//! through `World::eco_set`, with `regolith = 0`, and counts in
//! `weathered_cells`. A material without `weathers_to` stays at 255.
//!
//! [`season_tick`]: crate::tick::season_tick

use rmc_world::chunk::ChunkStorage;
use rmc_world::coords::{CellPos, LocalPos};
use rmc_world::layers::LayerId;
use rmc_world::materials::{MaterialId, Phase};

use crate::runner::{ChunkView, JacobiOut, JacobiPass};
use crate::tables::EcoTables;

/// Whether a temperature went from one side of 0 °C to the other:
/// `(T_before < 0) != (T_after < 0)`.
pub fn crossed_zero(before: i32, after: i32) -> bool {
    (before < 0) != (after < 0)
}

/// The six face neighbors.
const FACES: [[i32; 3]; 6] = [
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
    [-1, 0, 0],
    [1, 0, 0],
];

/// The fixed divisor of the release formula `total · Δ / 256`: a
/// composition total is what a full regolith cycle releases.
const RELEASE_DIV: i32 = 256;

/// The weathering pass (SAD-P2 §7.5), built once from the tables.
#[derive(Clone, Debug, Default)]
pub struct WeatheringPass {
    /// Per material id: `weathering_rate` of a solid material, 0 otherwise.
    rates: Vec<u8>,
    /// Per material id: `composition` as (substance index, total).
    composition: Vec<Vec<(u8, u8)>>,
    /// Per material id: the `weathers_to` material.
    weathers_to: Vec<Option<MaterialId>>,
}

impl WeatheringPass {
    pub fn new(tables: &EcoTables) -> Self {
        let materials = tables.materials();
        let rates = materials
            .iter()
            .map(|d| if d.is_solid() { d.weathering_rate } else { 0 })
            .collect();
        let composition = materials
            .iter()
            .map(|d| {
                d.composition
                    .iter()
                    .map(|(key, total)| {
                        let x = tables
                            .substance_index(key)
                            .expect("the tables validate composition keys");
                        (x, *total)
                    })
                    .collect()
            })
            .collect();
        let weathers_to = materials
            .iter()
            .map(|d| d.weathers_to.as_deref().and_then(|k| materials.id(k)))
            .collect();
        Self {
            rates,
            composition,
            weathers_to,
        }
    }

    /// `weathering_rate` of `m` if it is a solid that weathers, else 0.
    fn rate(&self, m: MaterialId) -> u8 {
        self.rates.get(usize::from(m.0)).copied().unwrap_or(0)
    }

    /// Whether `cell` has an air or liquid face neighbor (exposed) and
    /// whether one of them is water. Cells outside the bounds or in a
    /// non-resident chunk are no neighbor.
    fn surroundings(view: &ChunkView<'_>, cell: CellPos) -> (bool, bool) {
        let water = view.rules().water;
        let (mut exposed, mut by_water) = (false, false);
        for [x, y, z] in FACES {
            let n = CellPos::new(cell.0.x + x, cell.0.y + y, cell.0.z + z);
            let Some(m) = view.material(n) else {
                continue;
            };
            let open = view
                .materials()
                .get(m)
                .is_some_and(|d| matches!(d.phase, Phase::Air | Phase::Liquid));
            exposed |= open;
            by_water |= open && water == Some(m);
        }
        (exposed, by_water)
    }
}

impl JacobiPass for WeatheringPass {
    fn compute(&self, view: &ChunkView<'_>) -> JacobiOut {
        let mut out = JacobiOut::default();
        let chunk = view.chunk();
        // The palette lists every material the chunk may hold.
        let candidates = match &chunk.storage {
            ChunkStorage::Uniform(m) => self.rate(*m) > 0,
            ChunkStorage::Paletted { palette, .. } => palette.iter().any(|&m| self.rate(m) > 0),
        };
        if !candidates {
            return out;
        }
        let cap = i32::from(u8::MAX);
        for (i, cell) in view.cells() {
            let local = LocalPos::from_index(i);
            let m = chunk.storage.get(local);
            let rate = i32::from(self.rate(m));
            if rate == 0 {
                continue;
            }
            let id = usize::from(m.0);
            let weathers_to = self.weathers_to[id];
            let regolith = view.layer(cell, LayerId::Regolith);
            if regolith >= cap && weathers_to.is_none() {
                continue;
            }
            let (exposed, by_water) = Self::surroundings(view, cell);
            let moist = by_water || view.layer(cell, LayerId::Moisture) > 0;
            if !(exposed && moist) {
                continue;
            }
            let freeze_thaw = i32::from(chunk.freeze_thaw.contains(local));
            let delta = (rate * (1 + freeze_thaw)).min(cap - regolith);
            for &(x, total) in &self.composition[id] {
                let rel = i32::from(total) * delta / RELEASE_DIV;
                if rel == 0 {
                    continue;
                }
                let layer = LayerId::Substance(x);
                let got = rel.min(cap - view.layer(cell, layer));
                if got > 0 {
                    out.layers.push((i, layer, got));
                }
                out.ledger.weather_in[usize::from(x)] += i64::from(rel);
                out.ledger.saturation_loss[usize::from(x)] += i64::from(rel - got);
            }
            match weathers_to {
                Some(to) if regolith + delta == cap => {
                    out.materials.push((i, to));
                    out.layers.push((i, LayerId::Regolith, -regolith));
                    out.ledger.weathered_cells += 1;
                }
                _ if delta > 0 => out.layers.push((i, LayerId::Regolith, delta)),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, CellAabb};
    use rmc_world::ledger::SeasonLedger;
    use rmc_world::materials::{MaterialDef, MaterialTable};
    use rmc_world::world::World;

    use crate::activity::Activity;
    use crate::invariants::InvariantChecker;
    use crate::ledger::merge;
    use crate::runner::{Exec, activate_holders, run_jacobi};
    use crate::tables::{EcoSources, SeasonOverride};
    use crate::temperature::TemperaturePass;
    use crate::tick::season_tick;
    use crate::water::WaterPass;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    /// The `winter` row of `seasons.ron`, whose `air_temp` is below 0.
    const WINTER: u32 = 3;

    fn tables() -> EcoTables {
        EcoTables::load(ASSETS).expect("Phase 2 tables load")
    }

    /// Deterministic per-cell noise.
    fn noise(cell: CellPos, salt: u64) -> u64 {
        let c = cell.0;
        let mut h = salt ^ 0x9E37_79B9_7F4A_7C15;
        for v in [c.x, c.y, c.z] {
            h = (h ^ (v as u32 as u64)).wrapping_mul(0x100_0000_01B3);
            h ^= h >> 29;
        }
        h
    }

    /// Two chunks side by side; `cell` gives each in-bounds cell's material
    /// and temperature.
    fn world(t: &EcoTables, cell: impl Fn(CellPos) -> (MaterialId, i16)) -> World {
        let bounds = CellAabb::new(CellPos::new(-8, 0, 0), CellPos::new(7, 15, 7));
        let mut w = World::new(t.materials().clone(), bounds);
        w.set_eco_rules(t.displacement_rules());
        for pos in bounds.chunks() {
            let mut c = Chunk::uniform(MaterialId::AIR);
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                let at = CellPos::from_chunk_local(pos, l);
                if bounds.contains(at) {
                    let (m, temp) = cell(at);
                    c.storage.set(l, m);
                    c.layers.set_i16(LayerId::Temperature, l, temp);
                }
            }
            w.insert_chunk(pos, c).unwrap();
        }
        w
    }

    /// Mixed ground under an uneven surface, air above it, temperatures
    /// scattered around 0 so that many cells cross it in one pass.
    fn near_zero(t: &EcoTables, seed: u64) -> World {
        let id = |k: &str| t.materials().id(k).unwrap();
        let ground = [id("topsoil"), id("dirt"), id("stone"), id("water")];
        world(t, |c| {
            let h = noise(c, seed);
            let surface = 6 + (noise(CellPos::new(c.0.x, 0, c.0.z), seed) % 5) as i32;
            let m = if c.0.y <= surface {
                ground[(h % ground.len() as u64) as usize]
            } else {
                MaterialId::AIR
            };
            (m, ((h >> 16) % 301) as i16 - 150)
        })
    }

    fn cells(w: &World) -> Vec<CellPos> {
        let b = w.bounds();
        let mut v = Vec::new();
        for y in b.min.0.y..=b.max.0.y {
            for z in b.min.0.z..=b.max.0.z {
                for x in b.min.0.x..=b.max.0.x {
                    v.push(CellPos::new(x, y, z));
                }
            }
        }
        v
    }

    fn temp(w: &World, c: CellPos) -> i32 {
        w.layer(c, LayerId::Temperature)
    }

    fn bit(w: &World, c: CellPos) -> bool {
        let (pos, local) = c.split();
        w.chunk(pos).unwrap().freeze_thaw.contains(local)
    }

    fn bits(w: &World) -> usize {
        w.iter_chunks_sorted()
            .map(|(_, c)| c.freeze_thaw.len())
            .sum()
    }

    #[test]
    fn freeze_thaw_bit_set_on_zero_crossing() {
        assert!(crossed_zero(0, -1) && crossed_zero(-1, 0));
        assert!(!crossed_zero(0, 5) && !crossed_zero(-5, -1));

        let t = tables();
        let pass = TemperaturePass::new(&t, WINTER);
        assert!(pass.air_temp < 0, "the winter row freezes the surface");
        for seed in 0..4 {
            let mut w = near_zero(&t, seed);
            // A sky-open air cell stored above 0: its clamp to the winter
            // air_temp is a crossing, measured from the stored value.
            let sky = CellPos::new(3, 15, 3);
            w.eco_put_layer(sky, LayerId::Temperature, 50);
            // One already below 0 is clamped without crossing.
            let cold_sky = CellPos::new(-3, 15, 3);
            w.eco_put_layer(cold_sky, LayerId::Temperature, -50);
            let all = cells(&w);
            let mut serial = w.clone();
            let mut crossed = vec![false; all.len()];
            let (mut down, mut up, mut kept) = (0, 0, 0);
            for step in 0..3 {
                let before: Vec<i32> = all.iter().map(|&c| temp(&w, c)).collect();
                let mut ledger = SeasonLedger::default();
                run_jacobi(&mut w, &pass, Exec::Parallel, &mut ledger);
                run_jacobi(&mut serial, &pass, Exec::Serial, &mut ledger);
                for (i, &c) in all.iter().enumerate() {
                    let (b, a) = (before[i], temp(&w, c));
                    if crossed_zero(b, a) {
                        crossed[i] = true;
                        if a < 0 { down += 1 } else { up += 1 }
                    } else if b != a {
                        kept += 1;
                    }
                    // Set on a crossing in this pass, kept from earlier ones.
                    assert_eq!(
                        bit(&w, c),
                        crossed[i],
                        "seed {seed} step {step} {c:?}: {b} -> {a}"
                    );
                    assert_eq!(bit(&serial, c), bit(&w, c), "serial vs parallel");
                }
                if step == 0 {
                    assert_eq!(temp(&w, sky), i32::from(pass.air_temp));
                    assert!(bit(&w, sky), "seed {seed}: clamp crossing");
                    assert_eq!(temp(&w, cold_sky), i32::from(pass.air_temp));
                    assert!(!bit(&w, cold_sky), "seed {seed}: no crossing");
                }
            }
            assert!(down > 0 && up > 0 && kept > 0, "{down} {up} {kept}");
            // The bit is not part of the hash.
            assert_eq!(w.hash(), serial.hash());
        }
    }

    #[test]
    fn freeze_thaw_bit_cleared_at_season_start() {
        let t = tables();
        let stone = t.materials().id("stone").unwrap();
        let topsoil = t.materials().id("topsoil").unwrap();

        // A quiet world: all stone at the bottom clamp's temperature, so no
        // pass changes anything. Bits left from an earlier season go.
        let bottom = t.params().geothermal_base;
        let mut w = world(&t, |_| (stone, bottom));
        let marked = [
            CellPos::new(-8, 0, 0),
            CellPos::new(-1, 7, 3),
            CellPos::new(0, 7, 3),
            CellPos::new(7, 15, 7),
        ];
        for &c in &marked {
            assert!(w.eco_mark_freeze_thaw(c));
        }
        assert_eq!(bits(&w), marked.len());
        let hash = w.hash();
        let mut activity = Activity::new(t.params().quiet_skip);
        season_tick(&mut w, &t, WINTER, 0, &mut activity, Exec::Parallel);
        assert_eq!(w.hash(), hash, "nothing to compute");
        assert_eq!(bits(&w), 0);

        // Topsoil to y = 7 at 2 °C, air above it stored at 5 °C. The first
        // winter freezes the air clamps and the top of the ground; their
        // bits stay set to the end of the season.
        let mut w = world(&t, |c| {
            if c.0.y <= 7 {
                (topsoil, 20)
            } else {
                (MaterialId::AIR, 50)
            }
        });
        let air: Vec<CellPos> = cells(&w).into_iter().filter(|c| c.0.y > 7).collect();
        let mut activity = Activity::new(t.params().quiet_skip);
        season_tick(&mut w, &t, WINTER, 0, &mut activity, Exec::Parallel);
        for &c in &air {
            assert!(temp(&w, c) < 0);
            assert!(bit(&w, c), "{c:?}: the crossing's bit lasts the season");
        }
        // The next winter (row 3 again) starts with every bit clear. The air
        // stays at its clamp, so it does not cross again and ends the season
        // without a bit.
        season_tick(&mut w, &t, WINTER + 4, 0, &mut activity, Exec::Parallel);
        for &c in &air {
            assert!(!bit(&w, c), "{c:?}: an earlier season's bit is cleared");
        }
    }

    /// Test materials that weather: SAD-P2 §5 ships none before F140.
    /// They have no `weathers_to`, so a cell at regolith 255 stays at the
    /// cap and these tests see growth alone (conversion is tested apart).
    const WEATHERING: [&str; 2] = [
        r#"(id: 0, key: "weatherstone", phase: Solid, tags: ["stone"], hardness: 40,
            diggable: true, yield_count: 1, color: (128, 128, 128, 255), emissive: 0.0,
            weathering_rate: 3)"#,
        r#"(id: 0, key: "weatherchalk", phase: Solid, tags: ["stone", "porous"],
            hardness: 30, diggable: true, yield_count: 1, color: (200, 200, 180, 255), emissive: 0.0,
            permeability: 32, field_capacity: 24, weathering_rate: 5)"#,
    ];

    /// The `summer` row of `seasons.ron`: nothing near 15 °C freezes.
    const SUMMER: u32 = 1;

    /// The shipped tables plus the [`WEATHERING`] materials.
    fn wtables() -> EcoTables {
        tables_with(&WEATHERING)
    }

    /// The shipped tables plus test materials, appended in order.
    fn tables_with(extra: &[&str]) -> EcoTables {
        let base = MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap();
        let mut defs: Vec<MaterialDef> = base.iter().cloned().collect();
        for text in extra {
            let mut d: MaterialDef = ron::from_str(text).unwrap();
            d.id = MaterialId(defs.len() as u16);
            defs.push(d);
        }
        let m = Arc::new(MaterialTable::from_defs(defs).unwrap());
        EcoTables::from_sources(m, &EcoSources::read(ASSETS).unwrap()).unwrap()
    }

    struct Fix {
        t: EcoTables,
        granite: MaterialId,
        stone3: MaterialId,
        chalk5: MaterialId,
        water: MaterialId,
        ice: MaterialId,
    }

    fn fix() -> Fix {
        fix_with(wtables())
    }

    /// The fixture's materials in `t`, which holds the [`WEATHERING`] ones.
    fn fix_with(t: EcoTables) -> Fix {
        let id = |k: &str| t.materials().id(k).unwrap();
        let f = Fix {
            granite: id("granite"),
            stone3: id("weatherstone"),
            chalk5: id("weatherchalk"),
            water: id("water"),
            ice: id("ice"),
            t,
        };
        let rate = |m: MaterialId| f.t.materials().get(m).unwrap().weathering_rate;
        assert_eq!((rate(f.stone3), rate(f.chalk5), rate(f.granite)), (3, 5, 0));
        f
    }

    /// Granite up to y = 7 at 15 °C, air above, then `cells` placed with
    /// (material, moisture).
    fn ground(f: &Fix, cells: &[(CellPos, MaterialId, u8)]) -> World {
        let mut w = world(&f.t, |c| {
            let m = if c.0.y <= 7 {
                f.granite
            } else {
                MaterialId::AIR
            };
            (m, 150)
        });
        let mut ledger = SeasonLedger::default();
        for &(c, m, moisture) in cells {
            w.eco_set(c, m, &mut ledger);
            w.eco_put_layer(c, LayerId::Moisture, i32::from(moisture));
        }
        w
    }

    /// The tables with no rain in any season: the season tick's rain
    /// (SAD-P2 §7.3 D) would wet the dry cells these tests keep dry.
    fn no_rain(t: &EcoTables) -> EcoTables {
        t.with_season_override(SeasonOverride {
            rain: Some(0),
            air_temp: None,
        })
    }

    fn regolith(w: &World, c: CellPos) -> i32 {
        w.layer(c, LayerId::Regolith)
    }

    /// Runs the weathering pass once, in parallel and on a serial copy, and
    /// checks that both give the same world.
    fn weather(t: &EcoTables, w: &mut World) {
        let pass = WeatheringPass::new(t);
        let mut serial = w.clone();
        run_jacobi(w, &pass, Exec::Parallel, &mut SeasonLedger::default());
        run_jacobi(
            &mut serial,
            &pass,
            Exec::Serial,
            &mut SeasonLedger::default(),
        );
        assert_eq!(w.hash(), serial.hash(), "serial vs parallel");
    }

    /// Every in-bounds cell's regolith is 0 except `expect`.
    fn assert_regolith(w: &World, expect: &[(CellPos, i32)], what: &str) {
        for c in cells(w) {
            let want = expect.iter().find(|e| e.0 == c).map_or(0, |e| e.1);
            assert_eq!(regolith(w, c), want, "{what}: {c:?}");
        }
    }

    fn moisture(w: &World) -> Vec<i32> {
        cells(w)
            .iter()
            .map(|&c| w.layer(c, LayerId::Moisture))
            .collect()
    }

    #[test]
    fn regolith_only_when_exposed_and_moist() {
        let f = fix();
        let p = CellPos::new;
        // Surface cells at y = 7 under air; buried ones at y = 3 in granite.
        let dry_open = p(-6, 7, 1);
        let moist_open = p(-6, 7, 5);
        let under_water = p(-3, 7, 1);
        let moist_buried = p(-3, 3, 5);
        let by_water = p(3, 3, 1);
        let by_ice = p(3, 3, 5);
        let wet_granite = p(5, 7, 3);
        // Across the chunk face at x = 0: an air pocket in chunk 0 exposes
        // a moist cell in chunk -1, and a water cell in chunk -1 wets one in
        // chunk 0.
        let across_air = p(-1, 3, 3);
        let across_water = p(0, 5, 6);
        // On the world's edge: outside the bounds is no neighbor at all.
        let edge = p(-8, 3, 3);
        let floor = p(6, 0, 6);
        let cap = p(-6, 7, 3);
        let full = p(6, 7, 1);
        let mut w = ground(
            &f,
            &[
                (dry_open, f.stone3, 0),
                (moist_open, f.chalk5, 10),
                (under_water, f.stone3, 0),
                (p(-3, 8, 1), f.water, 200),
                (moist_buried, f.chalk5, 10),
                (by_water, f.stone3, 0),
                (p(3, 3, 2), f.water, 255),
                (by_ice, f.stone3, 0),
                (p(3, 3, 6), f.ice, 255),
                (wet_granite, f.granite, 10),
                (p(5, 8, 3), f.water, 255),
                (across_air, f.chalk5, 10),
                (p(0, 3, 3), MaterialId::AIR, 0),
                (across_water, f.stone3, 0),
                (p(-1, 5, 6), f.water, 255),
                (edge, f.chalk5, 10),
                (floor, f.chalk5, 10),
                (cap, f.chalk5, 10),
                (full, f.chalk5, 10),
            ],
        );
        w.eco_put_layer(cap, LayerId::Regolith, 253);
        w.eco_put_layer(full, LayerId::Regolith, 255);
        let before = moisture(&w);

        let grown = |n: i32| {
            vec![
                (moist_open, 5 * n),
                (under_water, 3 * n),
                (by_water, 3 * n),
                (across_air, 5 * n),
                (across_water, 3 * n),
                (cap, 255),
                (full, 255),
            ]
        };
        weather(&f.t, &mut w);
        assert_regolith(&w, &grown(1), "one pass");
        weather(&f.t, &mut w);
        assert_regolith(&w, &grown(2), "two passes");
        assert_eq!(before, moisture(&w), "weathering reads moisture only");

        // The season tick runs the pass once per sub-step. A summer pond
        // in a weatherstone pit: its floor and walls weather, the dry rim
        // does not, and nothing crosses 0 °C.
        let pit = p(2, 6, 4);
        let ring = [p(2, 5, 4), p(1, 6, 4), p(3, 6, 4), p(2, 6, 3), p(2, 6, 5)];
        let rim = [p(1, 7, 4), p(3, 7, 4), p(2, 7, 3), p(2, 7, 5)];
        let mut place: Vec<(CellPos, MaterialId, u8)> =
            ring.iter().chain(&rim).map(|&c| (c, f.stone3, 0)).collect();
        place.push((p(2, 7, 4), MaterialId::AIR, 0));
        place.push((pit, f.water, 255));
        let mut w = ground(&f, &place);
        activate_holders(&mut w, &WaterPass::new(&f.t));
        let mut activity = Activity::new(f.t.params().quiet_skip);
        season_tick(
            &mut w,
            &no_rain(&f.t),
            SUMMER,
            0,
            &mut activity,
            Exec::Parallel,
        );
        assert_eq!(w.get_cell(pit), Some(f.water));
        assert_eq!(bits(&w), 0, "summer: no crossings");
        let substeps = i32::from(f.t.seasons().substeps);
        let expect: Vec<(CellPos, i32)> = ring.iter().map(|&c| (c, 3 * substeps)).collect();
        assert_regolith(&w, &expect, "a summer");
    }

    #[test]
    fn freeze_thaw_doubles_rate() {
        let f = fix();
        let p = CellPos::new;
        let chalk = p(-6, 7, 2);
        let chalk_ft = p(-6, 7, 5);
        let stone = p(-2, 3, 2);
        let stone_ft = p(-2, 3, 5);
        let capped_ft = p(4, 7, 2);
        let buried_ft = p(4, 3, 5);
        // A bit on the air above a cell is not the cell's bit.
        let air_ft_over = p(6, 7, 6);
        let mut w = ground(
            &f,
            &[
                (chalk, f.chalk5, 10),
                (chalk_ft, f.chalk5, 10),
                (stone, f.stone3, 0),
                (p(-2, 3, 1), f.water, 255),
                (stone_ft, f.stone3, 0),
                (p(-2, 3, 6), f.water, 255),
                (capped_ft, f.chalk5, 10),
                (buried_ft, f.chalk5, 10),
                (air_ft_over, f.chalk5, 10),
            ],
        );
        w.eco_put_layer(capped_ft, LayerId::Regolith, 250);
        for c in [chalk_ft, stone_ft, capped_ft, buried_ft, p(6, 8, 6)] {
            assert!(w.eco_mark_freeze_thaw(c));
        }
        let expect = |n: i32| {
            vec![
                (chalk, 5 * n),
                (chalk_ft, 10 * n),
                (stone, 3 * n),
                (stone_ft, 6 * n),
                (capped_ft, 255),
                (air_ft_over, 5 * n),
            ]
        };
        weather(&f.t, &mut w);
        assert_regolith(&w, &expect(1), "one pass");
        weather(&f.t, &mut w);
        assert_regolith(&w, &expect(2), "two passes");

        // A bit the temperature pass sets mid-season doubles every later
        // pass of the season.
        assert!(w.eco_mark_freeze_thaw(chalk));
        weather(&f.t, &mut w);
        let mut e = expect(3);
        e[0].1 = 5 * 2 + 10;
        assert_regolith(&w, &e, "bit set after two passes");

        // Up to the cap and no further: 255 / 3 passes for the slowest.
        for _ in 0..u8::MAX / 3 {
            weather(&f.t, &mut w);
        }
        for c in [chalk, chalk_ft, stone, stone_ft, capped_ft, air_ft_over] {
            assert_eq!(regolith(&w, c), 255, "{c:?}");
        }
        assert_eq!(regolith(&w, buried_ft), 0);
    }

    #[test]
    fn unexposed_control_unchanged() {
        let f = fix();
        let p = CellPos::new;
        // A moist weatherchalk block at x -2..=1, y 2..=5, z 2..=5, across
        // the chunk face, buried in granite, every cell's bit set.
        let block: Vec<CellPos> = cells(&ground(&f, &[]))
            .into_iter()
            .filter(|c| (-2..=1).contains(&c.0.x) && (2..=5).contains(&c.0.y))
            .filter(|c| (2..=5).contains(&c.0.z))
            .collect();
        let place: Vec<(CellPos, MaterialId, u8)> =
            block.iter().map(|&c| (c, f.chalk5, 20)).collect();
        // Dry weatherstone in the open: on the surface and under the sky
        // at the world's top edge.
        let mut dry = place.clone();
        dry.push((p(-6, 7, 6), f.stone3, 0));
        dry.push((p(6, 15, 0), f.stone3, 0));
        let mut w = ground(&f, &dry);
        for &c in &block {
            assert!(w.eco_mark_freeze_thaw(c));
        }
        let hash = w.hash();
        let pass = WeatheringPass::new(&f.t);
        for _ in 0..3 {
            let mut ledger = SeasonLedger::default();
            let r = run_jacobi(&mut w, &pass, Exec::Parallel, &mut ledger);
            assert!(r.changed.is_empty(), "{:?}", r.changed);
            assert_eq!(ledger, SeasonLedger::default());
        }
        assert_eq!(w.hash(), hash, "unexposed and dry cells do not change");
        let mut activity = Activity::new(f.t.params().quiet_skip);
        season_tick(
            &mut w,
            &no_rain(&f.t),
            SUMMER,
            0,
            &mut activity,
            Exec::Parallel,
        );
        assert_regolith(&w, &[], "a summer");

        // The same block with one cell of its top face opened to an air
        // pocket: only the cell under the pocket weathers.
        let mut place = place;
        place.push((p(0, 6, 3), MaterialId::AIR, 0));
        let mut w = ground(&f, &place);
        weather(&f.t, &mut w);
        assert_regolith(&w, &[(p(0, 5, 3), 5)], "opened");
    }

    /// Test materials that weather, release their composition and convert
    /// (SAD-P2 §7.5); none ships before F140.
    const RELEASING: [&str; 4] = [
        r#"(id: 0, key: "releasestone", phase: Solid, tags: ["stone"], hardness: 40,
            diggable: true, yield_count: 1, color: (120, 110, 100, 255), emissive: 0.0,
            weathering_rate: 16, weathers_to: Some("dirt"),
            composition: [("cu", 60), ("fe", 40), ("s", 80)])"#,
        r#"(id: 0, key: "releasechalk", phase: Solid, tags: ["stone", "porous"],
            hardness: 30, diggable: true, yield_count: 1, color: (210, 210, 190, 255), emissive: 0.0,
            permeability: 32, field_capacity: 24, weathering_rate: 7,
            weathers_to: Some("dirt"), composition: [("lime", 120)])"#,
        r#"(id: 0, key: "keepstone", phase: Solid, tags: ["stone"], hardness: 40,
            diggable: true, yield_count: 1, color: (90, 90, 90, 255), emissive: 0.0,
            weathering_rate: 16, composition: [("cu", 60)])"#,
        r#"(id: 0, key: "richstone", phase: Solid, tags: ["stone"], hardness: 40,
            diggable: true, yield_count: 1, color: (140, 100, 60, 255), emissive: 0.0,
            weathering_rate: 32, weathers_to: Some("dirt"),
            composition: [("cu", 255), ("salt", 200)])"#,
    ];

    struct Rel {
        f: Fix,
        release: MaterialId,
        chalk: MaterialId,
        keep: MaterialId,
        rich: MaterialId,
        dirt: MaterialId,
        cu: u8,
        fe: u8,
        s: u8,
        lime: u8,
        salt: u8,
    }

    fn rfix_with(extra: &[&str]) -> Rel {
        let texts: Vec<&str> = WEATHERING
            .iter()
            .chain(&RELEASING)
            .chain(extra)
            .copied()
            .collect();
        let f = fix_with(tables_with(&texts));
        let id = |k: &str| f.t.materials().id(k).unwrap();
        let x = |k: &str| f.t.substance_index(k).unwrap();
        Rel {
            release: id("releasestone"),
            chalk: id("releasechalk"),
            keep: id("keepstone"),
            rich: id("richstone"),
            dirt: id("dirt"),
            cu: x("cu"),
            fe: x("fe"),
            s: x("s"),
            lime: x("lime"),
            salt: x("salt"),
            f,
        }
    }

    fn rfix() -> Rel {
        rfix_with(&[])
    }

    fn sub(w: &World, c: CellPos, x: u8) -> i32 {
        w.layer(c, LayerId::Substance(x))
    }

    /// Runs the weathering pass once, in parallel and on a serial copy;
    /// checks that both give the same world and ledger and returns the
    /// ledger.
    fn weather_ledger(t: &EcoTables, w: &mut World) -> SeasonLedger {
        let pass = WeatheringPass::new(t);
        let mut serial = w.clone();
        let mut ledger = SeasonLedger::default();
        let mut serial_ledger = SeasonLedger::default();
        run_jacobi(w, &pass, Exec::Parallel, &mut ledger);
        run_jacobi(&mut serial, &pass, Exec::Serial, &mut serial_ledger);
        assert_eq!(w.hash(), serial.hash(), "serial vs parallel");
        assert_eq!(ledger, serial_ledger, "serial vs parallel ledger");
        ledger
    }

    /// `cells` with a full water cell placed above each.
    fn under_water(f: &Fix, cells: &[(CellPos, MaterialId)]) -> Vec<(CellPos, MaterialId, u8)> {
        let mut v = Vec::new();
        for &(c, m) in cells {
            v.push((c, m, 0));
            v.push((CellPos::new(c.0.x, c.0.y + 1, c.0.z), f.water, 255));
        }
        v
    }

    #[test]
    fn release_proportional_to_actual_delta() {
        let r = rfix();
        let f = &r.f;
        let p = CellPos::new;
        let plain = p(-6, 7, 1);
        let doubled = p(-3, 7, 1);
        let capped = p(3, 7, 1);
        let stocked = p(6, 7, 1);
        let mut place = under_water(
            f,
            &[
                (plain, r.release),
                (doubled, r.release),
                (capped, r.release),
                (stocked, r.release),
            ],
        );
        // Porous chalk moist under the open air, on both sides of the chunk
        // face; and a moist buried cell and a cell at the cap that cannot
        // weather.
        let chalk = p(-1, 7, 5);
        let chalk_ft = p(0, 7, 5);
        let buried = p(-3, 3, 5);
        let full = p(5, 7, 5);
        place.extend([
            (chalk, r.chalk, 10),
            (chalk_ft, r.chalk, 10),
            (buried, r.release, 10),
            (full, r.keep, 10),
        ]);
        let mut w = ground(f, &place);
        for c in [doubled, chalk_ft] {
            assert!(w.eco_mark_freeze_thaw(c));
        }
        w.eco_put_layer(capped, LayerId::Regolith, 250);
        w.eco_put_layer(full, LayerId::Regolith, 255);
        w.eco_put_layer(stocked, LayerId::Substance(r.cu), 100);
        let checker = InvariantChecker::new(&w);
        let before = moisture(&w);

        let mut ledger = weather_ledger(&f.t, &mut w);
        // rel = total · Δ / 256, truncating, per substance.
        let cfs = |w: &World, c: CellPos| (sub(w, c, r.cu), sub(w, c, r.fe), sub(w, c, r.s));
        // Δ = 16: 960 / 256, 640 / 256, 1280 / 256.
        assert_eq!(cfs(&w, plain), (3, 2, 5));
        // Δ = 32 with the freeze-thaw bit.
        assert_eq!(cfs(&w, doubled), (7, 5, 10));
        // Δ = min(16, 255 − 250) = 5: the actual Δ, not the requested 16.
        assert_eq!(cfs(&w, capped), (1, 0, 1));
        // Release adds to what the cell holds.
        assert_eq!(cfs(&w, stocked), (103, 2, 5));
        // Lime: Δ = 7 gives 840 / 256, Δ = 14 gives 1680 / 256.
        assert_eq!(sub(&w, chalk, r.lime), 3);
        assert_eq!(sub(&w, chalk_ft, r.lime), 6);
        for c in [buried, full] {
            assert_eq!(cfs(&w, c), (0, 0, 0), "{c:?} does not weather");
        }
        checker.check(&w, &ledger).unwrap();

        // Each pass releases on its own Δ.
        merge(&mut ledger, &weather_ledger(&f.t, &mut w));
        assert_eq!(cfs(&w, plain), (6, 4, 10));
        assert_eq!(cfs(&w, doubled), (14, 10, 20));
        assert_eq!(sub(&w, chalk, r.lime), 6);
        assert_eq!(sub(&w, chalk_ft, r.lime), 12);
        assert_eq!(cfs(&w, full), (0, 0, 0));
        checker.check(&w, &ledger).unwrap();
        assert_eq!(before, moisture(&w), "weathering reads moisture only");
        // No other substance moves.
        for c in cells(&w) {
            for x in 0..t_substances(&f.t) {
                if ![r.cu, r.fe, r.s, r.lime].contains(&x) {
                    assert_eq!(sub(&w, c, x), 0, "{c:?} substance {x}");
                }
            }
        }
    }

    fn t_substances(t: &EcoTables) -> u8 {
        t.substances().len() as u8
    }

    #[test]
    fn release_complete_within_rounding() {
        // One material per rate, all with the same composition.
        let rates = [1u8, 2, 3, 5, 7, 16, 33, 48, 85, 128, 255];
        let comp = r#"[("cu", 60), ("fe", 40), ("s", 80), ("lime", 120), ("salt", 255),
            ("n", 1), ("p", 7)]"#;
        let texts: Vec<String> = rates
            .iter()
            .map(|r| {
                format!(
                    r#"(id: 0, key: "cycle{r}", phase: Solid, tags: ["stone"], hardness: 40,
                    diggable: true, yield_count: 1, color: (100, 100, 100, 255), emissive: 0.0,
                    weathering_rate: {r}, weathers_to: Some("dirt"), composition: {comp})"#
                )
            })
            .collect();
        let extra: Vec<&str> = texts.iter().map(String::as_str).collect();
        let r = rfix_with(&extra);
        let f = &r.f;
        let p = CellPos::new;
        let mut cases = Vec::new();
        for (k, rate) in rates.iter().enumerate() {
            for ft in [false, true] {
                let n = 2 * k + usize::from(ft);
                let c = p(-7 + 2 * (n % 8) as i32, 7, 1 + 2 * (n / 8) as i32);
                let m = f.t.materials().id(&format!("cycle{rate}")).unwrap();
                cases.push((c, m, *rate, ft));
            }
        }
        let place: Vec<(CellPos, MaterialId)> = cases.iter().map(|&(c, m, ..)| (c, m)).collect();
        let mut w = ground(f, &under_water(f, &place));
        for &(c, .., ft) in &cases {
            if ft {
                assert!(w.eco_mark_freeze_thaw(c));
            }
        }
        let composition: Vec<(u8, i32)> =
            f.t.materials()
                .get(cases[0].1)
                .unwrap()
                .composition
                .iter()
                .map(|(k, total)| (f.t.substance_index(k).unwrap(), i32::from(*total)))
                .collect();
        let checker = InvariantChecker::new(&w);
        let mut ledger = SeasonLedger::default();
        // Per cell: the Δ of every pass until it converts.
        let mut deltas: Vec<Vec<i32>> = vec![Vec::new(); cases.len()];
        for _ in 0..=u8::MAX {
            let reg: Vec<i32> = cases.iter().map(|&(c, ..)| regolith(&w, c)).collect();
            let was: Vec<MaterialId> = cases
                .iter()
                .map(|&(c, ..)| w.get_cell(c).unwrap())
                .collect();
            merge(&mut ledger, &weather_ledger(&f.t, &mut w));
            for (k, &(c, m, ..)) in cases.iter().enumerate() {
                if was[k] != m {
                    continue;
                }
                let now = w.get_cell(c).unwrap();
                let d = if now == m {
                    regolith(&w, c) - reg[k]
                } else {
                    i32::from(u8::MAX) - reg[k]
                };
                deltas[k].push(d);
            }
        }
        checker.check(&w, &ledger).unwrap();
        for (k, &(c, _, rate, ft)) in cases.iter().enumerate() {
            let step = i32::from(rate) * (1 + i32::from(ft));
            let n = deltas[k].len() as i32;
            assert_eq!(w.get_cell(c), Some(r.dirt), "{rate} {ft}: converted");
            assert_eq!(regolith(&w, c), 0);
            assert_eq!(
                n,
                (i32::from(u8::MAX) + step - 1) / step,
                "{rate} {ft}: steps"
            );
            assert_eq!(deltas[k].iter().sum::<i32>(), i32::from(u8::MAX));
            for &(x, total) in &composition {
                let got = sub(&w, c, x);
                // Exactly the truncated releases of the actual Δs.
                let want: i32 = deltas[k].iter().map(|d| total * d / 256).sum();
                assert_eq!(got, want, "{rate} {ft} substance {x}");
                // Never more than the total; short of it by under one unit
                // per step over the 255 regolith of a full cycle.
                assert!(got <= total, "{rate} {ft} {x}: {got} > {total}");
                assert!(
                    256 * got >= 255 * total - 255 * n,
                    "{rate} {ft} {x}: {got} of {total} in {n} steps"
                );
            }
        }
        // A whole-cell release (rate 255) gives total · 255 / 256.
        let (c, ..) = cases[2 * (rates.len() - 1)];
        assert_eq!(sub(&w, c, r.cu), 60 * 255 / 256);
        assert_eq!(sub(&w, c, r.salt), 254);
    }

    #[test]
    fn converts_at_255() {
        let r = rfix();
        let f = &r.f;
        let p = CellPos::new;
        let reaches = p(-6, 7, 1);
        let at_cap = p(-3, 7, 1);
        let not_yet = p(3, 7, 1);
        let keep_cap = p(5, 7, 5);
        let mut place = under_water(
            f,
            &[
                (reaches, r.release),
                (at_cap, r.release),
                (not_yet, r.release),
                (keep_cap, r.keep),
            ],
        );
        // Moist chalk under the air, across the chunk face from the others;
        // and a moist buried cell at the cap, which is not weathering.
        let chalk = p(0, 7, 5);
        let buried_cap = p(-3, 3, 5);
        place.extend([(chalk, r.chalk, 10), (buried_cap, r.release, 10)]);
        let mut w = ground(f, &place);
        for (c, reg) in [
            (reaches, 240),
            (at_cap, 255),
            (not_yet, 238),
            (keep_cap, 255),
            (chalk, 250),
            (buried_cap, 255),
        ] {
            w.eco_put_layer(c, LayerId::Regolith, reg);
        }
        let revision = |w: &World, c: CellPos| w.chunk(c.split().0).unwrap().revision;
        let rev0 = revision(&w, reaches);
        let checker = InvariantChecker::new(&w);

        let ledger = weather_ledger(&f.t, &mut w);
        // Growth to exactly 255 converts in the same pass; regolith resets
        // and the released substances stay on the cell.
        assert_eq!(w.get_cell(reaches), Some(r.dirt));
        assert_eq!(regolith(&w, reaches), 0);
        assert_eq!(sub(&w, reaches, r.cu), 60 * 15 / 256);
        assert_eq!(sub(&w, reaches, r.s), 80 * 15 / 256);
        assert!(revision(&w, reaches) > rev0, "a material change");
        // Already at the cap: Δ = 0, no release, converts.
        assert_eq!(w.get_cell(at_cap), Some(r.dirt));
        assert_eq!(regolith(&w, at_cap), 0);
        assert_eq!(sub(&w, at_cap, r.cu), 0);
        // Porous chalk keeps its moisture through the change.
        assert_eq!(w.get_cell(chalk), Some(r.dirt));
        assert_eq!(regolith(&w, chalk), 0);
        assert_eq!(w.layer(chalk, LayerId::Moisture), 10);
        assert_eq!(sub(&w, chalk, r.lime), 120 * 5 / 256);
        // 238 + 16 = 254: not yet.
        assert_eq!(w.get_cell(not_yet), Some(r.release));
        assert_eq!(regolith(&w, not_yet), 254);
        // No weathers_to: stays at the cap. Not weathering: no change.
        assert_eq!(w.get_cell(keep_cap), Some(r.keep));
        assert_eq!(regolith(&w, keep_cap), 255);
        assert_eq!(w.get_cell(buried_cap), Some(r.release));
        assert_eq!(regolith(&w, buried_cap), 255);
        assert_eq!(ledger.weathered_cells, 3);
        checker.check(&w, &ledger).unwrap();

        // 254 + min(16, 1): converts on the next pass.
        let ledger = weather_ledger(&f.t, &mut w);
        assert_eq!(w.get_cell(not_yet), Some(r.dirt));
        assert_eq!(regolith(&w, not_yet), 0);
        assert_eq!(ledger.weathered_cells, 1);

        // Nothing is left that weathers.
        let hash = w.hash();
        let pass = WeatheringPass::new(&f.t);
        let mut ledger = SeasonLedger::default();
        let report = run_jacobi(&mut w, &pass, Exec::Parallel, &mut ledger);
        assert!(report.changed.is_empty(), "{:?}", report.changed);
        assert_eq!(ledger, SeasonLedger::default());
        assert_eq!(w.hash(), hash);

        // In a season: a moist cell one step short converts in the first
        // sub-step, and the season's ledger counts it.
        let cell = p(2, 7, 4);
        let mut w = ground(f, &[(cell, r.release, 10)]);
        w.eco_put_layer(cell, LayerId::Regolith, 255 - 16);
        let checker = InvariantChecker::new(&w);
        let mut activity = Activity::new(f.t.params().quiet_skip);
        let report = season_tick(&mut w, &f.t, SUMMER, 0, &mut activity, Exec::Parallel);
        assert_eq!(w.get_cell(cell), Some(r.dirt));
        assert_eq!(regolith(&w, cell), 0);
        assert_eq!(report.ledger.weathered_cells, 1);
        checker.check(&w, &report.ledger).unwrap();
    }

    #[test]
    fn ledger_weather_in_and_saturation_loss() {
        let r = rfix();
        let f = &r.f;
        let p = CellPos::new;
        let near_full = p(-6, 7, 1);
        let full = p(1, 7, 1);
        let fresh = p(-2, 7, 5);
        let converts = p(4, 7, 5);
        let place = under_water(
            f,
            &[
                (near_full, r.rich),
                (full, r.rich),
                (fresh, r.release),
                (converts, r.rich),
            ],
        );
        let mut w = ground(f, &place);
        for (c, cu) in [(near_full, 240), (full, 255), (converts, 254)] {
            w.eco_put_layer(c, LayerId::Substance(r.cu), cu);
        }
        w.eco_put_layer(converts, LayerId::Regolith, 250);
        let checker = InvariantChecker::new(&w);

        let ledger = weather_ledger(&f.t, &mut w);
        // Rich stone, Δ = 32: cu 255 · 32 / 256 = 31, salt 200 · 32 / 256 = 25.
        // Δ = 5 on the converting cell: cu 4, salt 3.
        assert_eq!(sub(&w, near_full, r.cu), 255);
        assert_eq!(sub(&w, full, r.cu), 255);
        assert_eq!(sub(&w, converts, r.cu), 255);
        for (c, salt) in [(near_full, 25), (full, 25), (converts, 3)] {
            assert_eq!(sub(&w, c, r.salt), salt, "{c:?}");
        }
        assert_eq!(w.get_cell(converts), Some(r.dirt));
        let mut want = SeasonLedger::default();
        // Released: 31 + 31 + 3 (fresh releasestone) + 4. Of it, the cells
        // took 15 + 0 + 3 + 1; the rest found no room.
        want.weather_in[usize::from(r.cu)] = 31 + 31 + 3 + 4;
        want.saturation_loss[usize::from(r.cu)] = 16 + 31 + 3;
        want.weather_in[usize::from(r.salt)] = 25 + 25 + 3;
        want.weather_in[usize::from(r.fe)] = 2;
        want.weather_in[usize::from(r.s)] = 5;
        want.weathered_cells = 1;
        assert_eq!(ledger, want);
        checker.check(&w, &ledger).unwrap();

        // Over a season the terms accumulate and the invariant holds.
        let a = p(-5, 7, 3);
        let b = p(5, 7, 3);
        let mut w = ground(f, &[(a, r.rich, 10), (b, r.release, 10)]);
        w.eco_put_layer(a, LayerId::Substance(r.cu), 200);
        let checker = InvariantChecker::new(&w);
        let mut activity = Activity::new(f.t.params().quiet_skip);
        let report = season_tick(&mut w, &f.t, SUMMER, 0, &mut activity, Exec::Parallel);
        let l = &report.ledger;
        assert_eq!(sub(&w, a, r.cu), 255, "rich stone saturates");
        let cu = usize::from(r.cu);
        assert_eq!(
            l.weather_in[cu] - l.saturation_loss[cu],
            55 + i64::from(sub(&w, b, r.cu)),
            "released less lost is what the cells gained"
        );
        assert!(l.saturation_loss[cu] > 0);
        assert_eq!(
            l.weather_in[usize::from(r.salt)],
            i64::from(sub(&w, a, r.salt))
        );
        assert!(l.weather_in[usize::from(r.fe)] > 0);
        checker.check(&w, l).unwrap();
    }
}
