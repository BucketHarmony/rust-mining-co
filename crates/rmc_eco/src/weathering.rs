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

/// The weathering pass (SAD-P2 §7.5), built once from the tables.
#[derive(Clone, Debug, Default)]
pub struct WeatheringPass {
    /// Per material id: `weathering_rate` of a solid material, 0 otherwise.
    rates: Vec<u8>,
}

impl WeatheringPass {
    pub fn new(tables: &EcoTables) -> Self {
        let rates = tables
            .materials()
            .iter()
            .map(|d| if d.is_solid() { d.weathering_rate } else { 0 })
            .collect();
        Self { rates }
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
            let rate = i32::from(self.rate(chunk.storage.get(local)));
            if rate == 0 {
                continue;
            }
            let regolith = view.layer(cell, LayerId::Regolith);
            if regolith >= cap {
                continue;
            }
            let (exposed, by_water) = Self::surroundings(view, cell);
            let moist = by_water || view.layer(cell, LayerId::Moisture) > 0;
            if !(exposed && moist) {
                continue;
            }
            let freeze_thaw = i32::from(chunk.freeze_thaw.contains(local));
            let delta = (rate * (1 + freeze_thaw)).min(cap - regolith);
            out.layers.push((i, LayerId::Regolith, delta));
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
    use crate::runner::{Exec, activate_holders, run_jacobi};
    use crate::tables::EcoSources;
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
        season_tick(&mut w, &t, WINTER, &mut activity, Exec::Parallel);
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
        season_tick(&mut w, &t, WINTER, &mut activity, Exec::Parallel);
        for &c in &air {
            assert!(temp(&w, c) < 0);
            assert!(bit(&w, c), "{c:?}: the crossing's bit lasts the season");
        }
        // The next winter (row 3 again) starts with every bit clear. The air
        // stays at its clamp, so it does not cross again and ends the season
        // without a bit.
        season_tick(&mut w, &t, WINTER + 4, &mut activity, Exec::Parallel);
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
        let base = MaterialTable::load(format!("{ASSETS}/materials.ron")).unwrap();
        let mut defs: Vec<MaterialDef> = base.iter().cloned().collect();
        for text in WEATHERING {
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
        let t = wtables();
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
        season_tick(&mut w, &f.t, SUMMER, &mut activity, Exec::Parallel);
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
        season_tick(&mut w, &f.t, SUMMER, &mut activity, Exec::Parallel);
        assert_regolith(&w, &[], "a summer");

        // The same block with one cell of its top face opened to an air
        // pocket: only the cell under the pocket weathers.
        let mut place = place;
        place.push((p(0, 6, 3), MaterialId::AIR, 0));
        let mut w = ground(&f, &place);
        weather(&f.t, &mut w);
        assert_regolith(&w, &[(p(0, 5, 3), 5)], "opened");
    }
}
