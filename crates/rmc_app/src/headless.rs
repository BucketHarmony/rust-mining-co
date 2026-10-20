//! `rmc_app --headless --seasons N` (SAD-P2 §9): seasons with no client
//! and no window.
//!
//! [`run_headless`] starts the world as the game does ([`start_world`]:
//! the save in the save directory, else generated), builds a host with no
//! players over it, and advances N seasons. After each season it checks
//! the conservation invariants of SAD-P2 §7 against the season's ledger.
//! It then saves the world and writes [`REPORT_FILE`] into the save
//! directory. A violated invariant still writes both, then fails the run.

use std::collections::BTreeMap;

use anyhow::{Context, bail};
use rmc_eco::{InvariantChecker, Totals, Violation};
use rmc_host::ChannelTransport;
use rmc_protocol::SeasonReport;
use rmc_world::coords::ChunkPos;
use rmc_world::world::World as CellWorld;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::app::GameConfig;
use crate::session::{WorldOrigin, start_world};

/// The report's file name in the save directory.
pub const REPORT_FILE: &str = "report.json";

/// What a headless run writes to [`REPORT_FILE`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeadlessReport {
    pub seed: u64,
    /// The world's extent in cells, x, y, z.
    pub world_size: [i64; 3],
    pub chunks: usize,
    /// Whether the run continued the save (else it generated the world).
    pub loaded: bool,
    /// One entry per season, in order.
    pub seasons: Vec<SeasonEntry>,
    /// [`World::hash`](CellWorld::hash) after the last season.
    pub world_hash: u64,
    /// Chunks the save wrote.
    pub saved_chunks: usize,
    /// Whether every season kept every invariant.
    pub invariants_ok: bool,
}

/// One season of a headless run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeasonEntry {
    pub season: u32,
    /// Chunks whose cells or layers the season changed.
    pub changed_chunks: u32,
    /// The conserved totals before and after the season.
    pub before: Totals,
    pub after: Totals,
    /// Totals that the season's ledger does not explain; empty if none.
    pub violations: Vec<Violation>,
    /// What `SeasonDone` reports.
    pub report: SeasonReport,
}

/// Runs `seasons` seasons on the world `config` starts, saves it and
/// writes the report (see the module docs). `on_season` sees each season
/// as it ends.
pub fn run_headless(
    config: GameConfig,
    seasons: u32,
    mut on_season: impl FnMut(&SeasonEntry),
) -> anyhow::Result<HeadlessReport> {
    let GameConfig {
        assets,
        seed,
        save_dir,
    } = config;
    let start = start_world(&assets, seed, &save_dir)?;
    let loaded = start.origin == WorldOrigin::Loaded;
    let seed = start.host.seed;
    let mut host = start.into_host(Box::new(ChannelTransport::new()));
    let bounds = host.world().bounds();
    let world_size = (bounds.max.0 - bounds.min.0)
        .to_array()
        .map(|side| i64::from(side) + 1);
    info!(seed, seasons, ?world_size, loaded, "headless run started");

    let mut entries = Vec::new();
    for _ in 0..seasons {
        let season = host.season().context("the host runs no seasons")?;
        let checker = InvariantChecker::new(host.world());
        let before = revisions(host.world());
        let report = host.advance_season().context("the host runs no seasons")?;
        let world = host.world();
        let after = revisions(world);
        let changed = after.iter().filter(|(p, r)| before.get(p) != Some(r));
        let entry = SeasonEntry {
            season,
            changed_chunks: u32::try_from(changed.count()).unwrap_or(u32::MAX),
            before: *checker.before(),
            after: Totals::measure(world),
            violations: checker
                .check(world, &report.ledger)
                .err()
                .map_or_else(Vec::new, |v| v.0),
            report,
        };
        on_season(&entry);
        entries.push(entry);
    }

    let saved_chunks = host.save().context("saving after the seasons")?;
    let report = HeadlessReport {
        seed,
        world_size,
        chunks: host.world().chunk_count(),
        loaded,
        invariants_ok: entries.iter().all(|e| e.violations.is_empty()),
        seasons: entries,
        world_hash: host.world().hash(),
        saved_chunks,
    };
    let path = save_dir.join(REPORT_FILE);
    let json = serde_json::to_string_pretty(&report).context("encoding the report")?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    info!(path = %path.display(), saved_chunks, "headless run done");

    if !report.invariants_ok {
        let failed: Vec<u32> = report
            .seasons
            .iter()
            .filter(|e| !e.violations.is_empty())
            .map(|e| e.season)
            .collect();
        bail!(
            "invariants violated in seasons {failed:?}; see {}",
            path.display()
        );
    }
    Ok(report)
}

/// `(revision, eco_revision)` of every resident chunk.
fn revisions(world: &CellWorld) -> BTreeMap<ChunkPos, (u64, u64)> {
    world
        .iter_chunks_sorted()
        .map(|(pos, c)| (pos, (c.revision, c.eco_revision)))
        .collect()
}
