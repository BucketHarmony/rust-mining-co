//! Command-line arguments (SAD §10).

use std::path::PathBuf;

use clap::Parser;

/// Where the game saves and loads when `--save` is not given (SAD §10).
pub const DEFAULT_SAVE_DIR: &str = "save";

/// `rmc_app [--seed N] [--save DIR]`.
#[derive(Parser, Clone, Debug, PartialEq, Eq)]
#[command(name = "rmc_app", version, about = "Rust Mining Co")]
pub struct Cli {
    /// Worldgen seed. Without it: the save's seed if `--save` holds a save,
    /// else the default seed. A seed that differs from the save's fails the
    /// load.
    #[arg(long)]
    pub seed: Option<u64>,
    /// Save directory. Loaded on start if it holds a save; the autosave, the
    /// save on quit and the editor's Save write it.
    #[arg(long, default_value = DEFAULT_SAVE_DIR)]
    pub save: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("rmc_app").chain(args.iter().copied()))
    }

    #[test]
    fn parses_seed_and_save_path() {
        let seed_and_save = |cli: Cli| (cli.seed, cli.save);
        assert_eq!(
            seed_and_save(parse(&["--seed", "42", "--save", "worlds/one"]).unwrap()),
            (Some(42), PathBuf::from("worlds/one"))
        );
        // Either order, `=` form, the full u64 range, spaces in the path.
        assert_eq!(
            seed_and_save(parse(&["--save=my saves/a b", "--seed=18446744073709551615"]).unwrap()),
            (Some(u64::MAX), PathBuf::from("my saves/a b"))
        );
        // Defaults: no seed, the `save` directory.
        assert_eq!(
            seed_and_save(parse(&[]).unwrap()),
            (None, PathBuf::from(DEFAULT_SAVE_DIR))
        );
        assert_eq!(DEFAULT_SAVE_DIR, "save");
        assert_eq!(parse(&["--seed", "0"]).unwrap().seed, Some(0));
        // Refused: not a u64, missing values, unknown flags, stray args.
        for bad in [
            &["--seed", "abc"][..],
            &["--seed", "-1"],
            &["--seed", "18446744073709551616"],
            &["--seed"],
            &["--save"],
            &["--world", "x"],
            &["extra"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }
}
