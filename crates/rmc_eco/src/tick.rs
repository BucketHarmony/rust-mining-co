//! The season tick (SAD-P2 §3, §7): `seasons.substeps` sub-steps, each
//! running the passes in the fixed §3 order (temperature → transitions →
//! water → gases → weathering → chemistry → light → plants).
//!
//! The host calls [`season_tick`] with edits paused. Jacobi passes run
//! through the host's [`Activity`], which skips quiet chunks; sweeps run
//! over their active sets, which the caller fills after worldgen or a load
//! ([`activate_holders`]; the host does it when it installs a world). Only the passes that exist so far are run; each
//! later pass takes its place in the §3 order inside [`substep`].
//!
//! [`activate_holders`]: crate::runner::activate_holders

use std::time::Instant;

use rmc_world::ledger::SeasonLedger;
use rmc_world::world::World;

use crate::activity::{Activity, JacobiKind};
use crate::chemistry::{ReactionsPass, Transport};
use crate::gases::GasPass;
use crate::runner::{Exec, JacobiPass, run_sweep};
use crate::tables::EcoTables;
use crate::temperature::TemperaturePass;
use crate::transitions::TransitionsPass;
use crate::water::{Condensation, Evaporation, Rain, WaterPass};
use crate::weathering::WeatheringPass;

/// What one season tick did. Everything but `substep_ms` is a function of
/// the world, the tables, the season and the activity state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TickReport {
    /// Every ledger term of the season's passes.
    pub ledger: SeasonLedger,
    /// Wall time of each sub-step, in milliseconds.
    pub substep_ms: Vec<u32>,
    /// Cells visited by each sweep, in run order (sub-step major).
    pub active_cells_per_sweep: Vec<u32>,
    /// Chunks computed (not skipped as quiet) by each Jacobi pass, in run
    /// order (sub-step major).
    pub active_chunks_per_jacobi: Vec<u32>,
}

/// One pass of a sub-step, as [`season_tick_observed`] reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PassId {
    Temperature,
    Transitions,
    /// §7.3 A.
    Condensation,
    /// Flow sweep `i` of `eco.flow_iters` (§7.3 B).
    Flow(u8),
    /// §7.3 C.
    Evaporation,
    /// §7.3 D.
    Rain,
    /// The sweep of gas row `g` (§7.4).
    Gas(u8),
    Weathering,
    /// Chemistry transport, replaying the sub-step's water moves (§7.6).
    Transport,
    /// Chemistry reactions (§7.6).
    Reactions,
}

/// What a [`season_tick_observed`] observer sees after each pass: the
/// sub-step index, the pass, the world and the season's ledger so far.
pub type Observer<'a> = dyn FnMut(u8, PassId, &World, &SeasonLedger) + 'a;

/// The passes of one season, built once from the tables and the season's
/// row.
struct Passes {
    temperature: TemperaturePass,
    transitions: TransitionsPass,
    /// §7.3 A; `None` without a steam role.
    condensation: Option<Condensation>,
    water: WaterPass,
    /// §7.3 C; `None` without the steam, water and air roles.
    evaporation: Option<Evaporation>,
    /// §7.3 D.
    rain: Rain,
    /// One sweep per gas row, in row order.
    gases: Vec<GasPass>,
    weathering: WeatheringPass,
    /// §7.6 transport over the water pass's move log.
    transport: Transport,
    /// §7.6 reactions, after transport.
    reactions: ReactionsPass,
}

/// Runs season `season` (row `season % len` of `seasons.ron`) on `world`:
/// clears the freeze-thaw bits (SAD-P2 §7.5), marks the season boundary in
/// `activity`, then runs `seasons.substeps` sub-steps. `seed` is the world
/// seed; it patterns the rain (§7.3 D). `world` must carry the tables'
/// displacement rules ([`EcoTables::displacement_rules`]).
pub fn season_tick(
    world: &mut World,
    tables: &EcoTables,
    season: u32,
    seed: u64,
    activity: &mut Activity,
    exec: Exec,
) -> TickReport {
    season_tick_observed(
        world,
        tables,
        season,
        seed,
        activity,
        exec,
        &mut |_, _, _, _| {},
    )
}

/// [`season_tick`], calling `observe` after every pass of every sub-step
/// (SAD-P2 §7: the invariants are checked after every pass). A pass that
/// does not run (condensation or evaporation without their roles) is not
/// reported.
pub fn season_tick_observed(
    world: &mut World,
    tables: &EcoTables,
    season: u32,
    seed: u64,
    activity: &mut Activity,
    exec: Exec,
    observe: &mut Observer<'_>,
) -> TickReport {
    debug_assert!(
        *world.eco_rules() == tables.displacement_rules(),
        "world lacks the tables' displacement rules"
    );
    let mut passes = Passes {
        temperature: TemperaturePass::new(tables, season),
        transitions: TransitionsPass::new(tables),
        condensation: Condensation::new(tables),
        water: WaterPass::new(tables),
        evaporation: Evaporation::new(tables),
        rain: Rain::new(tables, season, seed),
        gases: GasPass::all(tables),
        weathering: WeatheringPass::new(tables),
        transport: Transport::new(tables),
        reactions: ReactionsPass::new(tables),
    };
    let mut report = TickReport::default();
    world.eco_clear_freeze_thaw();
    activity.season_boundary(world);
    for i in 0..tables.seasons().substeps {
        let start = Instant::now();
        let mut after =
            |pass: PassId, world: &World, ledger: &SeasonLedger| observe(i, pass, world, ledger);
        substep(world, &mut passes, activity, exec, &mut report, &mut after);
        let ms = start.elapsed().as_millis();
        report
            .substep_ms
            .push(u32::try_from(ms).unwrap_or(u32::MAX));
    }
    tracing::info!(
        season,
        substeps = report.substep_ms.len(),
        ms = report.substep_ms.iter().map(|&m| u64::from(m)).sum::<u64>(),
        "season tick done"
    );
    report
}

/// One sub-step: the passes in SAD-P2 §3 order. The water pass runs its
/// §7.3 steps in order: condensation, the flow sweeps, evaporation, rain.
/// Chemistry (§7.6) then replays the flow sweeps' move log and clears it
/// (transport), and runs the reactions with the cells evaporation left in
/// this sub-step. Only the flow and gas sweeps add an
/// `active_cells_per_sweep` entry.
/// `after` sees the world and ledger after each pass.
fn substep(
    world: &mut World,
    passes: &mut Passes,
    activity: &mut Activity,
    exec: Exec,
    report: &mut TickReport,
    after: &mut dyn FnMut(PassId, &World, &SeasonLedger),
) {
    jacobi(
        JacobiKind::Temperature,
        &passes.temperature,
        world,
        activity,
        exec,
        report,
    );
    after(PassId::Temperature, world, &report.ledger);
    jacobi(
        JacobiKind::Transitions,
        &passes.transitions,
        world,
        activity,
        exec,
        report,
    );
    after(PassId::Transitions, world, &report.ledger);
    if let Some(condensation) = &mut passes.condensation {
        run_sweep(world, condensation, &mut report.ledger);
        after(PassId::Condensation, world, &report.ledger);
    }
    for i in 0..passes.water.flow_iters() {
        passes.water.set_iter(i);
        let r = run_sweep(world, &mut passes.water, &mut report.ledger);
        report
            .active_cells_per_sweep
            .push(u32::try_from(r.visits).unwrap_or(u32::MAX));
        after(PassId::Flow(i), world, &report.ledger);
    }
    if let Some(evaporation) = &mut passes.evaporation {
        run_sweep(world, evaporation, &mut report.ledger);
        after(PassId::Evaporation, world, &report.ledger);
    }
    passes.rain.run(world, &mut report.ledger);
    after(PassId::Rain, world, &report.ledger);
    for (g, gas) in passes.gases.iter_mut().enumerate() {
        let r = run_sweep(world, gas, &mut report.ledger);
        report
            .active_cells_per_sweep
            .push(u32::try_from(r.visits).unwrap_or(u32::MAX));
        let g = u8::try_from(g).expect("gas rows fit the gas layers");
        after(PassId::Gas(g), world, &report.ledger);
    }
    jacobi(
        JacobiKind::Weathering,
        &passes.weathering,
        world,
        activity,
        exec,
        report,
    );
    after(PassId::Weathering, world, &report.ledger);
    passes.transport.run(world, passes.water.log());
    passes.water.clear_log();
    after(PassId::Transport, world, &report.ledger);
    let evaporated = passes
        .evaporation
        .as_mut()
        .map(Evaporation::take_evaporated)
        .unwrap_or_default();
    passes.reactions.set_evaporated(evaporated);
    jacobi(
        JacobiKind::Reactions,
        &passes.reactions,
        world,
        activity,
        exec,
        report,
    );
    after(PassId::Reactions, world, &report.ledger);
}

/// Runs one Jacobi pass through `activity` and records how many chunks it
/// computed.
fn jacobi(
    kind: JacobiKind,
    pass: &impl JacobiPass,
    world: &mut World,
    activity: &mut Activity,
    exec: Exec,
    report: &mut TickReport,
) {
    let r = activity.run(world, kind, pass, exec, &mut report.ledger);
    report
        .active_chunks_per_jacobi
        .push(u32::try_from(r.computed.len()).unwrap_or(u32::MAX));
}

#[cfg(test)]
mod tests {
    use super::*;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CHUNK_VOLUME, CellAabb, CellPos, LocalPos};
    use rmc_world::layers::{LayerId, N_GASES, N_SUBSTANCES};
    use rmc_world::materials::MaterialId;

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    /// Seeds in the determinism test.
    const SEEDS: u64 = 100;
    /// Every this many seeds, the runs cover two seasons (the season
    /// boundary in between) and a run from the next season row is
    /// compared; the others cover one season, for time.
    const LONG_EVERY: u64 = 10;

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

    /// A random world over two chunks side by side: random ground
    /// materials and temperatures below a seeded, uneven surface, air above
    /// it, and scattered overhangs.
    fn random_world(t: &EcoTables, seed: u64) -> World {
        let m = t.materials();
        let id = |k: &str| m.id(k).unwrap();
        let ground = [
            id("topsoil"),
            id("dirt"),
            id("stone"),
            id("granite"),
            id("bedrock"),
        ];
        let air = MaterialId::AIR;
        let bounds = CellAabb::new(CellPos::new(-12, 0, 0), CellPos::new(11, 23, 7));
        let mut w = World::new(m.clone(), bounds);
        w.set_eco_rules(t.displacement_rules());
        let base = 8 + (seed % 8) as i32;
        for pos in bounds.chunks() {
            let mut c = Chunk::uniform(air);
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                let at = CellPos::from_chunk_local(pos, l);
                if !bounds.contains(at) {
                    continue;
                }
                let h = noise(at, seed);
                let col = noise(CellPos::new(at.0.x, 0, at.0.z), seed) % 6;
                let surface = base + col as i32;
                let mat = if at.0.y <= surface || h.is_multiple_of(97) {
                    ground[(h % ground.len() as u64) as usize]
                } else {
                    air
                };
                c.storage.set(l, mat);
                let temp = ((h >> 20) % 3001) as i16 - 1000;
                c.layers.set_i16(LayerId::Temperature, l, temp);
            }
            w.insert_chunk(pos, c).unwrap();
        }
        w
    }

    /// Runs `seasons` seasons from season `first` and returns the world and
    /// every report with its timing cleared.
    fn run(
        t: &EcoTables,
        seed: u64,
        first: u32,
        seasons: u32,
        exec: Exec,
    ) -> (World, Vec<TickReport>) {
        let mut w = random_world(t, seed);
        let mut activity = Activity::new(t.params().quiet_skip);
        let reports = (first..first + seasons)
            .map(|season| {
                let mut r = season_tick(&mut w, t, season, seed, &mut activity, exec);
                assert_eq!(r.substep_ms.len(), usize::from(t.seasons().substeps));
                r.substep_ms.clear();
                r
            })
            .collect();
        (w, reports)
    }

    #[test]
    fn whole_tick_deterministic_100() {
        let t = tables();
        let substeps = usize::from(t.seasons().substeps);
        for seed in 0..SEEDS {
            let start = random_world(&t, seed).hash();
            let first = (seed % 4) as u32;
            let long = seed.is_multiple_of(LONG_EVERY);
            let seasons = if long { 2 } else { 1 };
            let (a, ra) = run(&t, seed, first, seasons, Exec::Parallel);
            let (b, rb) = run(&t, seed, first, seasons, Exec::Serial);
            assert_ne!(a.hash(), start, "seed {seed}: the tick changed nothing");
            assert_eq!(a.hash(), b.hash(), "seed {seed}: parallel vs serial");
            assert_eq!(ra, rb, "seed {seed}: reports, parallel vs serial");
            for (pos, chunk) in a.iter_chunks_sorted() {
                let other = b.chunk(pos).unwrap();
                assert_eq!(
                    (chunk.revision, chunk.eco_revision),
                    (other.revision, other.eco_revision),
                    "seed {seed}: revisions of {pos:?}"
                );
            }
            // One Jacobi entry per Jacobi pass per sub-step (temperature,
            // transitions, weathering and reactions so far), never more
            // chunks than the world has.
            for r in &ra {
                assert_eq!(r.active_chunks_per_jacobi.len(), 4 * substeps);
                assert!(r.active_chunks_per_jacobi.iter().all(|&n| n <= 2));
            }
            // The season row matters: the next row's `air_temp` gives a
            // different world.
            if long {
                let (d, _) = run(&t, seed, first + 1, seasons, Exec::Parallel);
                assert_ne!(a.hash(), d.hash(), "seed {seed}: season row ignored");
            }
        }
    }

    #[test]
    fn water_pass_condenses_evaporates_and_rains() {
        let t = tables();
        let steam = t.role_gas("steam").expect("steam role");
        let tick_from = |seed: u64| {
            let mut w = random_world(&t, 5);
            // Steam in every covered air cell (under the scattered
            // overhangs), where it can condense.
            let covered: Vec<CellPos> = crate::reference::global_order(w.bounds())
                .filter(|&c| {
                    w.get_cell(c) == Some(MaterialId::AIR)
                        && c.0.y < i32::from(w.surface_y(c.0.x, c.0.z))
                })
                .collect();
            assert!(!covered.is_empty(), "the random world has overhangs");
            for c in covered {
                w.eco_put_layer(c, LayerId::Gas(steam), 50);
            }
            let checker = crate::invariants::InvariantChecker::new(&w);
            let mut activity = Activity::new(t.params().quiet_skip);
            let r = season_tick(&mut w, &t, 0, seed, &mut activity, Exec::Serial);
            checker
                .check(&w, &r.ledger)
                .expect("conserved over the tick");
            (w, r)
        };
        let (a, r) = tick_from(7);
        let l = &r.ledger;
        assert!(l.condense_from_steam > 0, "covered steam condensed");
        assert!(l.rain_in > 0, "spring rain fell");
        assert!(l.evap_to_steam > 0, "the rain's puddles evaporated");
        // The rain pattern follows the world seed.
        assert_eq!(a.hash(), tick_from(7).0.hash());
        assert_ne!(a.hash(), tick_from(8).0.hash());
    }

    #[test]
    fn gas_sweeps_run_after_water() {
        // No rain (SAD-P2 §7.3 D): it would land on the open air the pocket
        // sits in and push the gas aside.
        let t = tables().with_season_override(crate::tables::SeasonOverride {
            rain: Some(0),
            air_temp: None,
        });
        let substeps = usize::from(t.seasons().substeps);
        let sweeps = usize::from(t.params().flow_iters) + t.gases().len();
        let mut w = random_world(&t, 3);
        // A pocket of a light gas in the air above the ground, under
        // another air cell and under an overhang, so that it is not
        // sky-open and does not vent (§7.4).
        let gas = t.role_gas("firedamp").expect("firedamp role");
        let is_air = |w: &World, c: CellPos| w.get_cell(c) == Some(MaterialId::AIR);
        let up = |c: CellPos| CellPos::new(c.0.x, c.0.y + 1, c.0.z);
        let covered = |w: &World, c: CellPos| c.0.y + 1 < i32::from(w.surface_y(c.0.x, c.0.z));
        let cell = crate::reference::global_order(w.bounds())
            .find(|&c| c.0.y >= 16 && is_air(&w, c) && is_air(&w, up(c)) && covered(&w, c))
            .expect("air under an overhang above the ground");
        w.eco_put_layer(cell, LayerId::Gas(gas), 200);
        let checker = crate::invariants::InvariantChecker::new(&w);
        let mut activity = Activity::new(t.params().quiet_skip);

        let r = season_tick(&mut w, &t, 0, 0, &mut activity, Exec::Serial);

        // Every sub-step: the flow sweeps, then one sweep per gas row.
        assert_eq!(r.active_cells_per_sweep.len(), substeps * sweeps);
        let gas_visits: u32 = r
            .active_cells_per_sweep
            .chunks(sweeps)
            .map(|s| s[usize::from(t.params().flow_iters) + usize::from(gas)])
            .sum();
        assert!(gas_visits > 0, "the gas sweep visited the pocket");
        assert!(w.layer(cell, LayerId::Gas(gas)) < 200, "the gas moved");
        assert!(w.layer(up(cell), LayerId::Gas(gas)) > 0, "and rose");
        checker
            .check(&w, &r.ledger)
            .expect("conserved over the tick");
    }

    /// Random worlds in the whole-tick invariant test.
    const INVARIANT_WORLDS: u64 = 20;
    /// Worlds of that test whose active sets are checked after every pass.
    const SETS_EVERY_PASS: u64 = 2;

    /// The shipped tables with rows appended to the material table: magma
    /// if the table lacks it (the SAD-P2 §5 row: Liquid, viscosity 3,
    /// conductivity 64, with F131's heat capacity 64), so that the magma
    /// swaps and transitions run, and the passable `moss` of plants.ron, so
    /// that rain finds plants.
    fn rich_tables() -> EcoTables {
        let base = std::fs::read_to_string(format!("{ASSETS}/materials.ron")).unwrap();
        let mut n = rmc_world::materials::MaterialTable::from_ron_str(&base)
            .unwrap()
            .len();
        let end = base.rfind(']').expect("material list closes");
        let mut rows = String::new();
        if !base.contains("key: \"magma\"") {
            rows.push_str(&format!(
                "(id: {n}, key: \"magma\", phase: Liquid, tags: [], hardness: 0, \
                 diggable: false, yield_count: 0, color: (255, 90, 0, 255), emissive: 2.0, \
                 viscosity: 3, conductivity: 64, heat_capacity: 64),\n"
            ));
            n += 1;
        }
        rows.push_str(&format!(
            "(id: {n}, key: \"moss\", phase: Solid, tags: [\"plant\"], hardness: 1, \
             diggable: true, yield_count: 1, color: (60, 140, 60, 255), emissive: 0.0, \
             passable: true),\n"
        ));
        let text = format!("{}{rows}]", &base[..end]);
        let m = std::sync::Arc::new(
            rmc_world::materials::MaterialTable::from_ron_str(&text).expect("test table loads"),
        );
        EcoTables::load_with_materials(m, ASSETS).expect("Phase 2 tables load")
    }

    /// A random world over 4 × 2 × 4 = 32 chunks, 72 × 8 × 72 cells. Its
    /// bounds cut every chunk face of the layout, so that water, gas and
    /// heat cross faces in x, y and z, negative coordinates included:
    /// - bedrock at the bottom, then random ground (soils, stones, the
    ///   porous limestone and crushed sulfide over and under capacity,
    ///   ice, water pockets, magma) up to an uneven surface with pits;
    /// - ponds of random fill in the pits, moss on some surface cells,
    ///   stray water and moist air, scattered overhangs;
    /// - every gas on some air cells; three seeded substances on some
    ///   cells (fewer layers keep the per-pass checks fast);
    /// - temperatures from frost to hot spots that boil water and melt
    ///   stone, and magma from just molten to well above setting.
    ///
    /// The sweeps' active sets hold every holder, as the host installs a
    /// world.
    fn rich_world(t: &EcoTables, seed: u64) -> World {
        let m = t.materials();
        let id = |k: &str| m.id(k).unwrap_or_else(|| panic!("material {k}"));
        let ground = [
            id("topsoil"),
            id("dirt"),
            id("clay"),
            id("stone"),
            id("granite"),
            id("basalt"),
            id("limestone"),
            id("crushed_sulfide"),
            id("coal_seam"),
        ];
        let top = [
            id("topsoil"),
            id("dirt"),
            id("limestone"),
            id("crushed_sulfide"),
        ];
        let (air, water, ice) = (MaterialId::AIR, id("water"), id("ice"));
        let (magma, moss, stone, bedrock) = (id("magma"), id("moss"), id("stone"), id("bedrock"));
        let porous = |mat: MaterialId| m.get(mat).is_some_and(|d| d.has_tag("porous"));
        let bounds = CellAabb::new(CellPos::new(-36, -4, -36), CellPos::new(35, 3, 35));
        assert_eq!(bounds.chunks().count(), 32);
        let mut w = World::new(m.clone(), bounds);
        w.set_eco_rules(t.displacement_rules());
        for pos in bounds.chunks() {
            let mut c = Chunk::uniform(air);
            for i in 0..CHUNK_VOLUME as u16 {
                let l = LocalPos::from_index(i);
                let at = CellPos::from_chunk_local(pos, l);
                if !bounds.contains(at) {
                    continue;
                }
                let y = at.0.y;
                let h = noise(at, seed);
                let col = noise(CellPos::new(at.0.x, 0, at.0.z), seed);
                let pit = col.is_multiple_of(11);
                let surface = (col % 4) as i32 - 1 - if pit { 2 } else { 0 };
                let r = |salt: u32| (h >> salt) % 256;
                let mat = if y == bounds.min.0.y {
                    bedrock
                } else if y < surface {
                    match r(8) % 40 {
                        0 => magma,
                        1 => ice,
                        2 => water,
                        k => ground[k as usize % ground.len()],
                    }
                } else if y == surface {
                    top[(r(8) % top.len() as u64) as usize]
                } else if pit && y <= surface + 2 {
                    water
                } else if y == surface + 1 && r(8) % 8 == 0 {
                    moss
                } else if y > surface + 2 && r(8) % 60 == 0 {
                    stone
                } else if r(8) % 25 == 0 {
                    water
                } else {
                    air
                };
                c.storage.set(l, mat);
                let v = r(16) as u8;
                let moisture = match mat {
                    _ if mat == water => v.max(1),
                    _ if mat == ice => v,
                    _ if mat == air && r(24) % 30 == 0 => v,
                    _ if porous(mat) => v,
                    _ => 0,
                };
                c.layers.set_u8(LayerId::Moisture, l, moisture);
                if mat == air {
                    for g in 0..t.gases().len() {
                        if r(24 + 4 * g as u32) % 6 == 0 {
                            c.layers
                                .set_u8(LayerId::Gas(g as u8), l, r(32 + g as u32) as u8);
                        }
                    }
                }
                if r(40) % 10 == 0 {
                    let x = (seed + r(44) % 3) % N_SUBSTANCES as u64;
                    let x = x as u8;
                    c.layers.set_u8(LayerId::Substance(x), l, r(48) as u8);
                }
                let temp = if mat == magma {
                    8000 + (r(52) * 20) as i16
                } else if r(52) % 80 == 0 {
                    12_800 + (r(56) * 4) as i16
                } else if r(52) % 80 == 1 {
                    900 + (r(56) * 8) as i16
                } else {
                    (r(56) * 3) as i16 - 300
                };
                c.layers.set_i16(LayerId::Temperature, l, temp);
            }
            w.insert_chunk(pos, c).unwrap();
        }
        crate::runner::activate_holders(&mut w, &crate::water::WaterPass::new(t));
        for gas in GasPass::all(t) {
            crate::runner::activate_holders(&mut w, &gas);
        }
        w
    }

    /// Every in-bounds cell that holds a sweep's quantity is in that
    /// sweep's active set (SAD-P2 §3), and gas sits only on air.
    fn assert_sweep_sets(w: &World, sweeps: &[&(dyn crate::runner::SweepPass + Sync)], at: &str) {
        for cell in crate::reference::global_order(w.bounds()) {
            let (pos, local) = cell.split();
            let chunk = w.chunk(pos).expect("resident");
            for pass in sweeps {
                assert!(
                    !pass.holds(w, cell) || chunk.active[pass.kind()].contains(local),
                    "{at}: {cell:?} holds the quantity of sweep {} but is not active",
                    pass.kind()
                );
            }
            if w.get_cell(cell) != Some(MaterialId::AIR) {
                for g in 0..N_GASES as u8 {
                    assert_eq!(
                        w.layer(cell, LayerId::Gas(g)),
                        0,
                        "{at}: gas {g} off air at {cell:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn invariants_hold_after_each_pass_20_worlds() {
        use rayon::prelude::*;

        let t = rich_tables();
        let water = crate::water::WaterPass::new(&t);
        let gases = GasPass::all(&t);
        let mut sweeps: Vec<&(dyn crate::runner::SweepPass + Sync)> = vec![&water];
        sweeps.extend(
            gases
                .iter()
                .map(|g| g as &(dyn crate::runner::SweepPass + Sync)),
        );
        let ledgers: Vec<SeasonLedger> = (0..INVARIANT_WORLDS)
            .into_par_iter()
            .map(|seed| {
                let mut w = rich_world(&t, seed);
                assert_sweep_sets(&w, &sweeps, &format!("seed {seed}, start"));
                let checker = crate::invariants::InvariantChecker::new(&w);
                let mut activity = Activity::new(t.params().quiet_skip);
                let mut observed = Vec::new();
                // Every season row in turn.
                let season = seed as u32;
                let r = season_tick_observed(
                    &mut w,
                    &t,
                    season,
                    seed,
                    &mut activity,
                    Exec::Parallel,
                    &mut |substep, pass, w, ledger| {
                        let at = format!("seed {seed}, sub-step {substep}, after {pass:?}");
                        if let Err(v) = checker.check(w, ledger) {
                            panic!("{at}: {v}");
                        }
                        // On the first worlds, after every pass. On the
                        // others, for time, after the passes other than the
                        // sweeps that change cells, each of which runs before
                        // a sweep: transitions before the water pass, rain
                        // before the gases, weathering before the next
                        // sub-step.
                        if seed < SETS_EVERY_PASS
                            || matches!(
                                pass,
                                PassId::Transitions | PassId::Rain | PassId::Weathering
                            )
                        {
                            assert_sweep_sets(w, &sweeps, &at);
                        }
                        observed.push((substep, pass));
                    },
                );
                // Every pass of every sub-step was observed, in §3 order.
                let mut want = Vec::new();
                for i in 0..t.seasons().substeps {
                    want.extend([
                        (i, PassId::Temperature),
                        (i, PassId::Transitions),
                        (i, PassId::Condensation),
                    ]);
                    want.extend((0..t.params().flow_iters).map(|f| (i, PassId::Flow(f))));
                    want.extend([(i, PassId::Evaporation), (i, PassId::Rain)]);
                    want.extend((0..t.gases().len() as u8).map(|g| (i, PassId::Gas(g))));
                    want.extend([
                        (i, PassId::Weathering),
                        (i, PassId::Transport),
                        (i, PassId::Reactions),
                    ]);
                }
                assert_eq!(observed, want, "seed {seed}: observed passes");
                r.ledger
            })
            .collect();
        // Together the random worlds drive every pass that exists so far.
        let sum = |f: &dyn Fn(&SeasonLedger) -> i64| ledgers.iter().map(f).sum::<i64>();
        let terms = [
            ("rain_in", sum(&|l| l.rain_in)),
            ("evap_to_steam", sum(&|l| l.evap_to_steam)),
            ("condense_from_steam", sum(&|l| l.condense_from_steam)),
            ("boil_to_steam", sum(&|l| l.boil_to_steam)),
            ("magma_set", sum(&|l| l.magma_set)),
            ("magma_melt_in", sum(&|l| l.magma_melt_in)),
            ("weather_in", sum(&|l| l.weather_in.iter().sum())),
            (
                "react_delta",
                sum(&|l| l.react_delta.iter().map(|v| v.abs()).sum()),
            ),
            ("transitions", sum(&|l| l.transitions.len() as i64)),
        ];
        for (name, v) in terms {
            assert!(v > 0, "no world exercised {name}");
        }
    }
}
