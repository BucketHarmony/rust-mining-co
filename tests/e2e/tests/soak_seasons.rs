//! Seasons soak (SAD-P2 §12): for each of three seeds, ChaosBot walks,
//! looks, digs, places and jumps for 600 frames on the default world, then
//! 20 seasons run. Nothing panics; after every season every conservation
//! invariant holds, both for the season alone and for everything since
//! before the chaos (each season's ledger plus the edits between seasons);
//! the replica hashes the same as the host after every season; and the
//! season tick's sub-step time has stabilized by season 4.
//!
//! The generated world lays no substances and no plants, so before the
//! chaos each seed builds a patch fixture in the middle of the world: damp
//! loam holding every substance, a row of each plant species on it, and a
//! damp crushed-sulfide pile beside it. The substance, plant and
//! weathering invariants then have something to conserve, and the chaos
//! plays where they are.
//!
//! "Stabilized" is read as a no-growth bound: every full cycle of the
//! season table after the first one from season 4 (seasons 4..8) takes at
//! most [`STABLE_NUM`] / [`STABLE_DEN`] times that cycle's wall-clock time.
//! Cycles run the same table rows, so the comparison is like for like.
//! Every season's sub-step times, active-cell counts and the ledger since
//! the start are recorded in `<out>/soak_seasons-stats.txt`.

use bevy::prelude::*;
use rmc_client::{MeshQueue, MeshStats, Replica};
use rmc_eco::InvariantChecker;
use rmc_eco::ledger::merge;
use rmc_testkit::bots::ChaosBot;
use rmc_testkit::{Fixture, TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellAabb, CellPos};
use rmc_world::edit::EditOp;
use rmc_world::layers::{LayerId, N_SUBSTANCES};
use rmc_world::ledger::SeasonLedger;
use rmc_world::shape::Shape;

/// The world and bot seeds (SAD-P2 §12: three seeds).
const SEEDS: [u64; 3] = [1, 2, 3];
/// Frames of chaos before the seasons (SAD-P2 §12).
const CHAOS_FRAMES: u32 = 600;
/// Seasons after the chaos (SAD-P2 §12).
const SEASONS: u32 = 20;
/// The season by which the sub-step time has stabilized (SAD-P2 §12).
const STABLE_BY: usize = 4;
/// The last cycle may take at most `STABLE_NUM / STABLE_DEN` of the first
/// stable cycle's time.
const STABLE_NUM: u128 = 3;
const STABLE_DEN: u128 = 2;
/// Steps the mesh queue may take to drain.
const DRAIN_FRAMES: u32 = 600;
/// Each substance laid in the patch's loam.
const PATCH_SUBSTANCE: i32 = 64;
/// Moisture laid in the patch and the pile: damp, under loam's field
/// capacity.
const DAMP: i32 = 24;
/// The plant species, one row each across the patch.
const PLANTS: [&str; 4] = ["grass", "fireweed", "moss", "shrub"];
/// The crushed-sulfide pile: site offsets of its square footprint, and its
/// height.
const PILE_D: (i32, i32) = (2, 7);
const PILE_HEIGHT: i32 = 2;

/// Steps until meshing is idle, nothing waits to upload and the replica
/// has no dirty chunk. Returns the steps taken.
fn drain(t: &mut TestApp, limit: u32) -> u32 {
    for n in 0..=limit {
        let world = t.app().world();
        let stats = world.resource::<MeshStats>();
        if stats.initial_load_done
            && world.resource::<MeshQueue>().is_idle()
            && stats.awaiting_upload == 0
            && world.resource::<Replica>().dirty().is_empty()
        {
            return n;
        }
        if n < limit {
            t.step();
        }
    }
    let world = t.app().world();
    panic!(
        "the mesh queue did not drain in {limit} steps: {:?}, {} dirty",
        world.resource::<MeshStats>(),
        world.resource::<Replica>().dirty().len()
    )
}

fn replica_hash(t: &TestApp) -> u64 {
    t.app().world().resource::<Replica>().world().hash()
}

#[test]
#[ignore]
fn soak_seasons() {
    let mut record = String::new();
    for seed in SEEDS {
        let clock = std::time::Instant::now();
        eprintln!("soak_seasons: seed {seed}");
        // The default world; a quarter of the default pixels keeps the
        // chaos affordable on a software adapter.
        let mut t = TestApp::new(TestConfig {
            seed,
            size: (320, 180),
            ..TestConfig::default()
        });
        e2e::run_scenario(&mut t, "soak_seasons", |t| {
            record.push_str(&soak(t, seed));
        });
        eprintln!("soak_seasons: seed {seed} done in {:.0?}", clock.elapsed());
    }
    let dir = e2e::out_dir();
    std::fs::create_dir_all(&dir).expect("create the E2E output directory");
    std::fs::write(dir.join("soak_seasons-stats.txt"), &record).expect("write the soak record");
}

/// Builds the patch fixture in the middle of the world and lays the
/// ecology on it (see the module docs).
fn seed_ecology(t: &mut TestApp) {
    let materials = t.host_world().materials().clone();
    let key = |k: &str| {
        materials
            .id(k)
            .unwrap_or_else(|| panic!("no material {k:?}"))
    };
    let site = t.fixture(Fixture::Patch(key("loam"), 0, 0));
    let patch = site.feature;
    let shape = |b: CellAabb| Shape::Box {
        min: b.min,
        max: b.max,
    };
    let damp = |b: CellAabb| {
        (
            EditOp::SetLayer {
                layer: LayerId::Moisture,
                value: DAMP,
            },
            shape(b),
        )
    };
    let mut edits: Vec<(EditOp, Shape)> = (0..N_SUBSTANCES)
        .map(|x| {
            (
                EditOp::SetLayer {
                    layer: LayerId::Substance(x as u8),
                    value: PATCH_SUBSTANCE,
                },
                shape(patch),
            )
        })
        .collect();
    edits.push(damp(patch));
    let (x0, z0) = (site.site.min.0.x, site.site.min.0.z);
    let pile = CellAabb::new(
        CellPos::new(x0 + PILE_D.0, site.ground_y + 1, z0 + PILE_D.0),
        CellPos::new(x0 + PILE_D.1, site.ground_y + PILE_HEIGHT, z0 + PILE_D.1),
    );
    edits.push((EditOp::Set(key("crushed_sulfide")), shape(pile)));
    edits.push(damp(pile));
    t.apply_edits(&edits);

    // A row of each species on top of the patch.
    let (min, max) = (patch.min.0, patch.max.0);
    let rows = (max.z - min.z + 1) / PLANTS.len() as i32;
    let plants: Vec<(EditOp, Shape)> = PLANTS
        .iter()
        .enumerate()
        .map(|(i, &k)| {
            let z = min.z + i as i32 * rows;
            let row = CellAabb::new(
                CellPos::new(min.x, max.y + 1, z),
                CellPos::new(max.x, max.y + 1, z + rows - 1),
            );
            (EditOp::Set(key(k)), shape(row))
        })
        .collect();
    t.apply_edits(&plants);
    let planted: usize = t
        .host_world()
        .iter_chunks_sorted()
        .map(|(_, c)| c.plants.len())
        .sum();
    eprintln!("seeded: patch {patch:?}, pile {pile:?}, {planted} plants");
    assert!(planted > 0, "the brush planted the patch");
}

/// One seed: chaos, then the seasons. Returns the seed's record.
fn soak(t: &mut TestApp, seed: u64) -> String {
    let clock = std::time::Instant::now();
    drain(t, DRAIN_FRAMES);

    // The default spawn: on top of the middle column.
    let bounds = t.config().world_bounds;
    let (min, max) = (bounds.min.0, bounds.max.0);
    let (cx, cz) = (
        min.x + (max.x - min.x + 1) / 2,
        min.z + (max.z - min.z + 1) / 2,
    );
    let top = (min.y..=max.y)
        .rev()
        .find(|&y| t.host_world().is_solid(CellPos::new(cx, y, cz)))
        .expect("the middle column has ground");
    t.spawn_player(Vec3::new(
        (cx as f32 + 0.5) * CELL_SIZE_M,
        (top + 1) as f32 * CELL_SIZE_M,
        (cz as f32 + 0.5) * CELL_SIZE_M,
    ));
    t.steps(30);
    drain(t, DRAIN_FRAMES);

    // Everything from here on is explained by the ledgers.
    let start = InvariantChecker::new(t.host_world());
    seed_ecology(t);
    let before_chaos = t.host_world().hash();
    let report = ChaosBot::new(seed, CHAOS_FRAMES)
        .run(t)
        .unwrap_or_else(|e| panic!("ChaosBot: {e}"));
    assert_eq!(report.frames, CHAOS_FRAMES, "the bot ran every frame");
    assert!(!t.editor().open, "the bot closed the editor");
    let drained = drain(t, DRAIN_FRAMES);
    t.step();
    eprintln!(
        "[{:.0?}] seed {seed}: chaos done at frame {}, drained in {drained} steps: {}",
        clock.elapsed(),
        t.frame(),
        report.summary()
    );
    assert_ne!(
        t.host_world().hash(),
        before_chaos,
        "the chaos changed the world"
    );
    assert_eq!(replica_hash(t), t.host_world().hash(), "after the chaos");
    let shot = t.screenshot();
    e2e::save_screenshot(&shot, &format!("soak_seasons-s{seed}-chaos"), t.frame());

    let mut record = format!("seed {seed}: {}\n", report.summary());
    let mut since_start = SeasonLedger::default();
    let mut season_ms = Vec::new();
    for n in 0..SEASONS {
        let checker = InvariantChecker::new(t.host_world());
        t.advance_season();
        let frames = t.last_season().expect("the season ended").clone();
        let r = &frames.report;
        merge(&mut since_start, &r.between_seasons);
        merge(&mut since_start, &r.ledger);
        let world = t.host_world();
        if let Err(v) = checker.check(world, &r.ledger) {
            panic!(
                "seed {seed}, season {}: the season's invariants: {v}",
                frames.season
            );
        }
        if let Err(v) = start.check(world, &since_start) {
            panic!(
                "seed {seed}, season {}: the invariants since before the chaos: {v}",
                frames.season
            );
        }
        if n > 0 {
            assert_eq!(
                r.between_seasons,
                SeasonLedger::default(),
                "seed {seed}, season {}: nothing was edited between seasons",
                frames.season
            );
        }
        // The client applied every snapshot before `SeasonDone`.
        assert_eq!(
            replica_hash(t),
            t.host_world().hash(),
            "seed {seed}, season {}: host and replica hashes",
            frames.season
        );
        let ms: u128 = r.substep_ms.iter().map(|&m| u128::from(m)).sum();
        let active: u64 = r.active_cells_per_sweep.iter().map(|&c| u64::from(c)).sum();
        let jacobi: u64 = r
            .active_chunks_per_jacobi
            .iter()
            .map(|&c| u64::from(c))
            .sum();
        let line = format!(
            "seed {seed} season {} ms {ms} substep_ms {:?} active_cells {active} \
             jacobi_chunks {jacobi} changed_chunks {} rain_in {} evap_to_steam {} \
             steam_vent_out {}\n",
            frames.season,
            r.substep_ms,
            frames.changed_chunks,
            r.ledger.rain_in,
            r.ledger.evap_to_steam,
            r.ledger.steam_vent_out,
        );
        eprint!("[{:.0?}] {line}", clock.elapsed());
        record.push_str(&line);
        season_ms.push(ms);
    }
    record.push_str(&format!("seed {seed} ledger since start {since_start:?}\n"));

    // Sub-step time has stabilized by season 4: no later cycle of the
    // season table costs more than the bound times the first stable one.
    let cycle = t.eco_tables().seasons().seasons.len();
    assert!(
        STABLE_BY + cycle <= season_ms.len() - cycle,
        "{SEASONS} seasons hold two cycles of {cycle} from season {STABLE_BY}"
    );
    let cycles: Vec<(usize, u128)> = (STABLE_BY..=season_ms.len() - cycle)
        .step_by(cycle)
        .map(|s| (s, season_ms[s..s + cycle].iter().sum()))
        .collect();
    let line = format!("seed {seed} cycles (first season, ms): {cycles:?}\n");
    eprint!("{line}");
    record.push_str(&line);
    let first = cycles[0].1;
    for &(s, ms) in &cycles[1..] {
        assert!(
            ms * STABLE_DEN <= first * STABLE_NUM,
            "seed {seed}: the sub-step time grows after season {STABLE_BY}: \
             {first} ms in seasons {STABLE_BY}..{}, {ms} ms in seasons {s}..{}; \
             per season {season_ms:?}",
            STABLE_BY + cycle,
            s + cycle
        );
    }

    // The seeded ecology took part: plants took substances up and the
    // pile weathered.
    let total = |terms: &[i64]| terms.iter().sum::<i64>();
    assert!(
        total(&since_start.uptake_sink) > 0,
        "seed {seed}: no plant took a substance up: {since_start:?}"
    );
    assert!(
        total(&since_start.weather_in) > 0,
        "seed {seed}: nothing weathered: {since_start:?}"
    );

    drain(t, DRAIN_FRAMES);
    assert_eq!(t.render_failure(), None);
    let shot = t.screenshot();
    let path = e2e::save_screenshot(&shot, &format!("soak_seasons-s{seed}-seasons"), t.frame());
    eprintln!("[{:.0?}] after: {}", clock.elapsed(), path.display());
    record
}
