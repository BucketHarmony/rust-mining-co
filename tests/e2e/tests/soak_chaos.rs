//! Soak (SAD §12.3, §12.4): ChaosBot walks, looks, digs, places and jumps
//! at random for 3,600 frames on the default world. Nothing panics, the
//! host and the replica hash the same at the end, the mesh queue drains,
//! and the step time's p95 is recorded against the 33 ms target (not
//! asserted: a software adapter's frame time is not the game's).
//!
//! `RMC_SOAK_FRAMES` sets the length and `RMC_SOAK_SEED` the seed of both
//! the world and the bot (`scripts/soak.sh`).

use std::time::Duration;

use bevy::prelude::*;
use rmc_client::{MeshQueue, MeshStats, Replica};
use rmc_testkit::bots::{ChaosAction, ChaosBot};
use rmc_testkit::{TestApp, TestConfig, e2e};
use rmc_world::coords::{CELL_SIZE_M, CellPos};

/// Frames of chaos without `RMC_SOAK_FRAMES` (SAD §12.4).
const DEFAULT_FRAMES: u32 = 3600;
/// Seed without `RMC_SOAK_SEED`.
const DEFAULT_SEED: u64 = 1;
/// The step-time target the p95 is recorded against (SAD §12.4).
const P95_TARGET: Duration = Duration::from_millis(33);
/// Steps the mesh queue may take to drain once the chaos stops.
const DRAIN_FRAMES: u32 = 600;

fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    match std::env::var(name) {
        Ok(v) => v
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{name}={v:?} does not parse")),
        Err(_) => default,
    }
}

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
fn soak_chaos() {
    let frames = env_or("RMC_SOAK_FRAMES", DEFAULT_FRAMES);
    let seed = env_or("RMC_SOAK_SEED", DEFAULT_SEED);
    eprintln!("soak_chaos: seed {seed}, {frames} frames");
    // The default world; a quarter of the default pixels keeps 3,600
    // frames affordable on a software adapter.
    let mut t = TestApp::new(TestConfig {
        seed,
        size: (320, 180),
        ..TestConfig::default()
    });
    e2e::run_scenario(&mut t, "soak_chaos", |t| soak(t, seed, frames));
}

fn soak(t: &mut TestApp, seed: u64, frames: u32) {
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
    let before = t.host_world().hash();
    let shot = t.screenshot();
    e2e::save_screenshot(&shot, "soak_chaos", t.frame());
    eprintln!("[{:.0?}] spawned: {:?}", clock.elapsed(), t.player());

    let report = ChaosBot::new(seed, frames)
        .run(t)
        .unwrap_or_else(|e| panic!("ChaosBot: {e}"));
    eprintln!(
        "[{:.0?}] chaos done at frame {}: {}",
        clock.elapsed(),
        t.frame(),
        report.summary()
    );
    assert_eq!(report.frames, frames, "the bot ran every frame");
    assert_eq!(report.step_times.len(), frames as usize);
    if frames >= DEFAULT_FRAMES {
        // A full soak does each of the things SAD §12.3 lists, and changes
        // the world.
        for kind in ChaosAction::ALL {
            assert!(report.count(kind) > 0, "no {kind:?} in {report:?}");
        }
        assert!(report.swings > 0, "the pickaxe swung");
        assert!(report.brush_clicks > 0, "the brush placed");
        assert_ne!(t.host_world().hash(), before, "the chaos changed the world");
    }
    assert!(!t.editor().open, "the bot closed the editor");

    // The mesh queue drains, and host and replica agree on every cell.
    let drained = drain(t, DRAIN_FRAMES);
    t.step();
    eprintln!("[{:.0?}] drained in {drained} steps", clock.elapsed());
    assert_eq!(
        replica_hash(t),
        t.host_world().hash(),
        "host and replica hashes"
    );
    assert_eq!(t.render_failure(), None);

    // Recorded, not asserted (SAD §12.4).
    let (p50, p95, worst) = (
        report.step_time_percentile(50),
        report.step_time_percentile(95),
        report.step_time_percentile(100),
    );
    let verdict = if p95 < P95_TARGET { "within" } else { "over" };
    let record = format!(
        "soak_chaos seed={seed} frames={frames} p50_ms={:.2} p95_ms={:.2} max_ms={:.2} \
         target_ms={} ({verdict} target) penetrating_frames={} {}\n",
        p50.as_secs_f64() * 1e3,
        p95.as_secs_f64() * 1e3,
        worst.as_secs_f64() * 1e3,
        P95_TARGET.as_millis(),
        report.penetrating_frames,
        report.summary(),
    );
    eprint!("{record}");
    let dir = e2e::out_dir();
    std::fs::create_dir_all(&dir).expect("create the E2E output directory");
    std::fs::write(dir.join("soak_chaos-stats.txt"), &record).expect("write the soak record");

    let shot = t.screenshot();
    let path = e2e::save_screenshot(&shot, "soak_chaos", t.frame());
    eprintln!("[{:.0?}] after: {}", clock.elapsed(), path.display());
}
