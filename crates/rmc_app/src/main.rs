//! Rust Mining Co binary: wires the host thread and the Bevy client, or
//! runs seasons headless (`--headless --seasons N`).
#![deny(unsafe_code)]

use std::time::Duration;

use anyhow::Context;
use bevy::prelude::AppExit;
use clap::Parser;
use rmc_app::app::{GameConfig, build_game_app};
use rmc_app::cli::Cli;
use rmc_app::eco_clock::EcoClock;
use rmc_app::headless::{REPORT_FILE, run_headless};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = GameConfig::from_cli(&cli)?;
    if cli.headless {
        let seasons = cli.seasons.context("--headless needs --seasons")?;
        let report = run_headless(config, seasons, |s| {
            let ms: u32 = s.report.substep_ms.iter().sum();
            println!(
                "season {}: {} chunks changed, {ms} ms",
                s.season, s.changed_chunks
            );
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
    app.insert_resource(EcoClock::new(Some(Duration::from_millis(cli.eco_tick_ms))));
    match app.run() {
        AppExit::Success => Ok(()),
        AppExit::Error(code) => anyhow::bail!("the game exited with code {code}"),
    }
}
