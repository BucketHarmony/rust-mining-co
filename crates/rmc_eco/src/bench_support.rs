//! The bodies of the SAD-P2 §13 benches (`tick_quiet`, `tick_first`,
//! `water_sweep_world`, `chem_transport_world`) and their setup, as plain
//! functions: `benches/` times them on the default 32-chunk world and
//! `tests/benches_run_once.rs` runs each once on a tiny one. Results are in
//! `docs/perf-p2.md`.
//!
//! The world comes from worldgen, which this crate cannot depend on; the
//! callers generate it and hand it in.

use rmc_world::coords::{CHUNK_SIZE, CellPos};
use rmc_world::edit::{CommandId, EditCommand, EditOp, EditStatus, PlayerId};
use rmc_world::layers::{LayerId, N_SUBSTANCES};
use rmc_world::ledger::SeasonLedger;
use rmc_world::shape::Shape;
use rmc_world::world::World;

use crate::activity::Activity;
use crate::chemistry::{MoveLog, Transport, TransportReport};
use crate::gases::GasPass;
use crate::runner::{Exec, SweepReport, activate_holders, run_sweep};
use crate::tables::{EcoTables, SeasonOverride};
use crate::tick::{TickReport, season_tick};
use crate::water::{Rain, WaterPass};

/// Quiet seasons [`settle`] runs before `tick_quiet` is timed.
pub const SETTLE_SEASONS: u32 = 2;

/// What [`add_substances`] puts in every cell, of every substance.
pub const SUBSTANCE_AMOUNT: u8 = 64;

/// The issuer of [`add_substances`]' edits.
const BENCH_PLAYER: PlayerId = PlayerId(0);

/// `tables` with every season row quiet (SAD-P2 §13): rain 0 and the
/// `air_temp` of row 0, the row worldgen lays the surface temperature from.
pub fn quiet_tables(tables: &EcoTables) -> EcoTables {
    tables.with_season_override(SeasonOverride {
        rain: Some(0),
        air_temp: Some(tables.seasons().seasons[0].air_temp),
    })
}

/// Prepares a generated world for seasons as the host installs one: the
/// tables' displacement rules, every water and gas holder in its sweep's
/// active set, and a fresh [`Activity`], which it returns.
pub fn install(world: &mut World, tables: &EcoTables) -> Activity {
    world.set_eco_rules(tables.displacement_rules());
    activate_holders(world, &WaterPass::new(tables));
    for gas in GasPass::all(tables) {
        activate_holders(world, &gas);
    }
    Activity::new(tables.params().quiet_skip)
}

/// Runs seasons `0..SETTLE_SEASONS` of the `quiet` tables on an installed
/// world, so that what worldgen left unsettled has moved before the quiet
/// tick is timed. Returns the next season.
pub fn settle(world: &mut World, quiet: &EcoTables, seed: u64, activity: &mut Activity) -> u32 {
    for season in 0..SETTLE_SEASONS {
        season_tick(world, quiet, season, seed, activity, Exec::Parallel);
    }
    SETTLE_SEASONS
}

/// `tick_quiet`: season `season` of the `quiet` tables
/// ([`quiet_tables`]) on an installed, settled world.
pub fn tick_quiet(
    world: &mut World,
    quiet: &EcoTables,
    season: u32,
    seed: u64,
    activity: &mut Activity,
) -> TickReport {
    season_tick(world, quiet, season, seed, activity, Exec::Parallel)
}

/// `tick_first`: installs a freshly generated world and runs season 0 of
/// `tables`, with its rain.
pub fn tick_first(world: &mut World, tables: &EcoTables, seed: u64) -> TickReport {
    let mut activity = install(world, tables);
    season_tick(world, tables, 0, seed, &mut activity, Exec::Parallel)
}

/// `water_sweep_world`: one flow sweep of `water`, at its current
/// iteration, over an installed world.
pub fn water_sweep_world(world: &mut World, water: &mut WaterPass) -> SweepReport {
    run_sweep(world, water, &mut SeasonLedger::default())
}

/// Sets every substance layer of every cell of `world` to `amount`, by
/// `SetLayer` edits, one per substance row and chunk (an edit covers at
/// most a chunk's cells).
pub fn add_substances(world: &mut World, tables: &EcoTables, amount: u8) {
    let bounds = world.bounds();
    let rows = tables.substances().len().min(N_SUBSTANCES);
    let mut seq = 0u32;
    for x in 0..rows {
        let layer = LayerId::Substance(u8::try_from(x).expect("substance rows fit a u8"));
        for pos in bounds.chunks() {
            let origin = pos.origin_cell().0;
            let last = origin + (CHUNK_SIZE - 1);
            seq += 1;
            let r = world.apply(&EditCommand {
                id: CommandId {
                    issuer: BENCH_PLAYER,
                    seq,
                },
                issuer: BENCH_PLAYER,
                op: EditOp::SetLayer {
                    layer,
                    value: i32::from(amount),
                },
                shape: Shape::Box {
                    min: CellPos(origin.max(bounds.min.0)),
                    max: CellPos(last.min(bounds.max.0)),
                },
            });
            assert_eq!(r.status, EditStatus::Applied, "substance {x}, {pos:?}");
        }
    }
}

/// The moves `chem_transport_world` replays: rains season 0 of `tables` on
/// an installed world, then runs one sub-step's flow sweeps
/// (`eco.flow_iters`) and returns their move log, as the tick hands it to
/// transport.
pub fn rained_moves(world: &mut World, tables: &EcoTables, seed: u64) -> MoveLog {
    let mut ledger = SeasonLedger::default();
    Rain::new(tables, 0, seed).run(world, &mut ledger);
    let mut water = WaterPass::new(tables);
    for i in 0..water.flow_iters() {
        water.set_iter(i);
        run_sweep(world, &mut water, &mut ledger);
    }
    water.log().clone()
}

/// `chem_transport_world`: chemistry transport replaying `log`.
pub fn chem_transport_world(
    world: &mut World,
    transport: &Transport,
    log: &MoveLog,
) -> TransportReport {
    transport.run(world, log)
}
