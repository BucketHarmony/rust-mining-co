//! Builds the garden level (see `rmc_app::garden`) into a save directory
//! that the game loads with `rmc_app --save <dir>`. Opens no window.
//! `--survey <dir>` instead counts the plants of a save (after playing it,
//! or a headless run) and what the bare surface lacks.
#![deny(unsafe_code)]

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use rmc_app::cli::DEFAULT_WORLD_SIZE;
use rmc_app::garden::{self, Survey, survey};
use rmc_app::session::{DEFAULT_SEED, GameAssets, assets_dir, load_world};

/// `build_garden [--save saves/garden] [--survey DIR]`.
#[derive(Parser)]
struct Args {
    /// Where to write the save; the game loads it with `--save <dir>`.
    #[arg(long, default_value = "saves/garden")]
    save: PathBuf,
    /// Survey this save instead of building.
    #[arg(long, value_name = "DIR")]
    survey: Option<PathBuf>,
}

fn print(label: &str, s: &Survey) {
    println!("{label}: {} surface sites, plants {:?}", s.sites, s.plants);
    if !s.parts.is_empty() {
        println!("  plant parts: {:?}", s.parts);
    }
    for (k, fails) in &s.fails {
        println!("  bare sites failing {k}'s needs: {fails:?}");
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let assets = GameAssets::load(&assets_dir())?.with_world_bounds(DEFAULT_WORLD_SIZE.bounds())?;
    if let Some(dir) = args.survey {
        let world = load_world(&assets, DEFAULT_SEED, &dir)?;
        print(&dir.display().to_string(), &survey(&world, &assets.eco));
        return Ok(());
    }
    let built = garden::build(&assets, DEFAULT_SEED)?;
    let chunks = rmc_world::save::save(&args.save, &assets.save_header(DEFAULT_SEED), &built.world)
        .with_context(|| format!("saving to {}", args.save.display()))?;
    println!(
        "garden: {} loam columns, planted {:?}",
        built.columns, built.planted
    );
    print("garden", &survey(&built.world, &assets.eco));
    println!("saved {chunks} chunks to {}", args.save.display());
    println!(
        "play: cargo run -p rmc_app -- --save {}",
        args.save.display()
    );
    Ok(())
}
