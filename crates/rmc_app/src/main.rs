//! Rust Mining Co binary: wires the host thread and the Bevy client, or
//! runs seasons headless (`--headless --seasons N`).
#![deny(unsafe_code)]

use anyhow::Context;
use bevy::prelude::AppExit;
use clap::Parser;
use rmc_app::app::{GameConfig, build_game_app};
use rmc_app::cli::Cli;
use rmc_app::eco_clock::EcoClock;
use rmc_app::headless::{REPORT_FILE, run_headless};
use rmc_app::session::{DEFAULT_SEED, assets_dir};
use rmc_app::tree_lab::run_tree_lab;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Some(species) = &cli.tree_lab {
        let years = cli.years.context("--tree-lab needs --years")?;
        let seed = cli.seed.unwrap_or(DEFAULT_SEED);
        let lab = run_tree_lab(&assets_dir(), species, seed, years, cli.flushes, &cli.out)?;
        println!(
            "{species}: {} frames, {} metamers, written to {}",
            lab.frames,
            lab.metamers,
            cli.out.display()
        );
        return Ok(());
    }
    let config = GameConfig::from_cli(&cli)?;
    let hours = config.calendar.as_ref().map(|c| c.hours);
    if cli.headless {
        let seasons = cli.seasons.context("--headless needs --seasons")?;
        let report = run_headless(config, seasons, |s| {
            let ms: u32 = s.report.substep_ms.iter().sum();
            let v = &s.vegetation;
            println!(
                "season {}: {} chunks changed, {ms} ms",
                s.season, s.changed_chunks
            );
            println!(
                "  {} plants over {} columns, biomass {}, parts {:?}",
                v.plant_count(),
                v.cover,
                v.biomass,
                v.parts
            );
            println!("  plants {:?}", v.plants);
            println!("  soil {:?}, limits {:?}", v.soil, v.limits);
        })?;
        println!(
            "{} seasons on seed {}: world hash {:016x}, {} chunks saved, {} written to {}",
            report.seasons.len(),
            report.seed,
            report.world_hash,
            report.saved_chunks,
            REPORT_FILE,
            cli.save.display()
        );
        return Ok(());
    }
    let mut app = build_game_app(config, true)?;
    let interval = cli.eco_interval(hours);
    app.insert_resource(EcoClock::new(interval));
    if let Some(interval) = interval {
        // The time of day moves smoothly between the host's hourly clocks.
        app.insert_resource(rmc_client::ClockPace(interval));
    }
    match app.run() {
        AppExit::Success => Ok(()),
        AppExit::Error(code) => anyhow::bail!("the game exited with code {code}"),
    }
}
