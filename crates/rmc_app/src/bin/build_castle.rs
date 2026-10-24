//! Builds the castle level (see `rmc_app::castle`) into a save directory
//! that the game loads with `rmc_app --save <dir>`. Opens no window.
#![deny(unsafe_code)]

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use rmc_app::castle;
use rmc_app::cli::DEFAULT_WORLD_SIZE;
use rmc_app::session::{DEFAULT_SEED, GameAssets, assets_dir};

/// `build_castle [--save saves/castle]`.
#[derive(Parser)]
struct Args {
    /// Where to write the save; the game loads it with `--save <dir>`.
    #[arg(long, default_value = "saves/castle")]
    save: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let assets = GameAssets::load(&assets_dir())?.with_world_bounds(DEFAULT_WORLD_SIZE.bounds())?;
    let built = castle::build(&assets, DEFAULT_SEED)?;
    let chunks = rmc_world::save::save(&args.save, &assets.save_header(DEFAULT_SEED), &built.world)
        .with_context(|| format!("saving to {}", args.save.display()))?;
    println!(
        "castle: {} commands, {} chunk deltas; ground y={}, water y={}, mine floor y={}",
        built.commands, built.changed, built.ground, built.water, built.mine_floor
    );
    let water = assets.eco.role_material("water");
    let b = built.world.bounds();
    let wet = (b.min.0.y..=b.max.0.y)
        .flat_map(|y| (b.min.0.z..=b.max.0.z).map(move |z| (y, z)))
        .flat_map(|(y, z)| (b.min.0.x..=b.max.0.x).map(move |x| (x, y, z)))
        .filter(|&(x, y, z)| {
            built
                .world
                .get_cell(rmc_world::coords::CellPos::new(x, y, z))
                == water
        })
        .count();
    println!("water cells: {wet}");
    println!("saved {chunks} chunks to {}", args.save.display());
    println!(
        "play: cargo run -p rmc_app -- --save {}",
        args.save.display()
    );
    Ok(())
}
