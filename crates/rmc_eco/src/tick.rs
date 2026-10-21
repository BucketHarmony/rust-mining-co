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
use crate::gases::GasPass;
use crate::runner::{Exec, JacobiPass, run_sweep};
use crate::tables::EcoTables;
use crate::temperature::TemperaturePass;
use crate::transitions::TransitionsPass;
use crate::water::WaterPass;
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

/// The passes of one season, built once from the tables and the season's
/// row.
struct Passes {
    temperature: TemperaturePass,
    transitions: TransitionsPass,
    water: WaterPass,
    /// One sweep per gas row, in row order.
    gases: Vec<GasPass>,
    weathering: WeatheringPass,
}

/// Runs season `season` (row `season % len` of `seasons.ron`) on `world`:
/// clears the freeze-thaw bits (SAD-P2 §7.5), marks the season boundary in
/// `activity`, then runs `seasons.substeps` sub-steps. `world` must carry the tables'
/// displacement rules ([`EcoTables::displacement_rules`]).
pub fn season_tick(
    world: &mut World,
    tables: &EcoTables,
    season: u32,
    activity: &mut Activity,
    exec: Exec,
) -> TickReport {
    debug_assert!(
        *world.eco_rules() == tables.displacement_rules(),
        "world lacks the tables' displacement rules"
    );
    let mut passes = Passes {
        temperature: TemperaturePass::new(tables, season),
        transitions: TransitionsPass::new(tables),
        water: WaterPass::new(tables),
        gases: GasPass::all(tables),
        weathering: WeatheringPass::new(tables),
    };
    let mut report = TickReport::default();
    world.eco_clear_freeze_thaw();
    activity.season_boundary(world);
    for _ in 0..tables.seasons().substeps {
        let start = Instant::now();
        substep(world, &mut passes, activity, exec, &mut report);
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

/// One sub-step: the passes in SAD-P2 §3 order.
fn substep(
    world: &mut World,
    passes: &mut Passes,
    activity: &mut Activity,
    exec: Exec,
    report: &mut TickReport,
) {
    jacobi(
        JacobiKind::Temperature,
        &passes.temperature,
        world,
        activity,
        exec,
        report,
    );
    jacobi(
        JacobiKind::Transitions,
        &passes.transitions,
        world,
        activity,
        exec,
        report,
    );
    for i in 0..passes.water.flow_iters() {
        passes.water.set_iter(i);
        let r = run_sweep(world, &mut passes.water, &mut report.ledger);
        report
            .active_cells_per_sweep
            .push(u32::try_from(r.visits).unwrap_or(u32::MAX));
    }
    for gas in &mut passes.gases {
        let r = run_sweep(world, gas, &mut report.ledger);
        report
            .active_cells_per_sweep
            .push(u32::try_from(r.visits).unwrap_or(u32::MAX));
    }
    jacobi(
        JacobiKind::Weathering,
        &passes.weathering,
        world,
        activity,
        exec,
        report,
    );
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
    use rmc_world::layers::LayerId;
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
                let mut r = season_tick(&mut w, t, season, &mut activity, exec);
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
            // transitions and weathering so far), never more chunks than the
            // world has.
            for r in &ra {
                assert_eq!(r.active_chunks_per_jacobi.len(), 3 * substeps);
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
    fn gas_sweeps_run_after_water() {
        let t = tables();
        let substeps = usize::from(t.seasons().substeps);
        let sweeps = usize::from(t.params().flow_iters) + t.gases().len();
        let mut w = random_world(&t, 3);
        // A pocket of a light gas in the open air, under another air cell.
        let gas = t.role_gas("firedamp").expect("firedamp role");
        let is_air = |w: &World, c: CellPos| w.get_cell(c) == Some(MaterialId::AIR);
        let up = |c: CellPos| CellPos::new(c.0.x, c.0.y + 1, c.0.z);
        let cell = crate::reference::global_order(w.bounds())
            .find(|&c| c.0.y >= 16 && is_air(&w, c) && is_air(&w, up(c)))
            .expect("open air above the ground");
        w.eco_put_layer(cell, LayerId::Gas(gas), 200);
        let checker = crate::invariants::InvariantChecker::new(&w);
        let mut activity = Activity::new(t.params().quiet_skip);

        let r = season_tick(&mut w, &t, 0, &mut activity, Exec::Serial);

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
}
