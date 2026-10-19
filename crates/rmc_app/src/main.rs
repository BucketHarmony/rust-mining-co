//! Rust Mining Co binary: wires the host thread and the Bevy client.
#![deny(unsafe_code)]

use bevy::prelude::AppExit;
use clap::Parser;
use rmc_app::app::{GameConfig, build_game_app};
use rmc_app::cli::Cli;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = GameConfig::from_cli(&cli)?;
    match build_game_app(config, true)?.run() {
        AppExit::Success => Ok(()),
        AppExit::Error(code) => anyhow::bail!("the game exited with code {code}"),
    }
}
