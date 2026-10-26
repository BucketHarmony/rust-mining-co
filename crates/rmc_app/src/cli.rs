//! Command-line arguments (SAD §10, SAD-P2 §9).

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use clap::Parser;
use rmc_world::coords::{CHUNK_SIZE, CellAabb, CellPos};

/// Where the game saves and loads when `--save` is not given (SAD §10).
pub const DEFAULT_SAVE_DIR: &str = "save";

/// The Phase 2 world: 128 × 64 × 128 cells, 4 × 2 × 4 = 32 chunks
/// (SAD-P2 §2).
pub const DEFAULT_WORLD_SIZE: WorldSize = WorldSize {
    width: 128,
    height: 64,
    depth: 128,
};

/// `rmc_app [--seed N] [--save DIR] [--world-size WxHxD]
/// [--headless --seasons N] [--eco-tick-ms MS]`.
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
    /// World size in cells, `WxHxD`, each a multiple of 32 (one chunk).
    /// Without it: the save's size if `--save` holds a save, else
    /// 128x64x128. A size that differs from the save's fails the load.
    #[arg(long, value_name = "WxHxD")]
    pub world_size: Option<WorldSize>,
    /// Run seasons with no client and no window, write the save and
    /// `report.json` into the save directory, and exit (SAD-P2 §9).
    #[arg(long, requires = "seasons")]
    pub headless: bool,
    /// Seasons a `--headless` run advances.
    #[arg(long, value_name = "N", requires = "headless")]
    pub seasons: Option<u32>,
    /// Milliseconds of real time between ecology sub-steps while playing
    /// (a season is 8 sub-steps); 0 turns the ecology clock off.
    #[arg(long, value_name = "MS", default_value_t = 1000)]
    pub eco_tick_ms: u64,
}

/// A world's extent in cells, from the origin. Each side is a positive
/// multiple of [`CHUNK_SIZE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldSize {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
}

impl WorldSize {
    /// The sides, x then y then z.
    pub fn sides(self) -> [u32; 3] {
        [self.width, self.height, self.depth]
    }

    /// How many chunks the world holds.
    pub fn chunks(self) -> u64 {
        self.sides()
            .iter()
            .map(|&s| u64::from(s / CHUNK_SIZE as u32))
            .product()
    }

    /// The cells `(0, 0, 0)..=(width − 1, height − 1, depth − 1)`.
    pub fn bounds(self) -> CellAabb {
        let [w, h, d] = self.sides().map(|s| s as i32 - 1);
        CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(w, h, d))
    }

    /// The size of `bounds` if it is a world from the origin with sides
    /// that are multiples of a chunk.
    pub fn of_bounds(bounds: CellAabb) -> Option<Self> {
        if bounds.min != CellPos::new(0, 0, 0) {
            return None;
        }
        let cells = |max: i32| {
            u64::try_from(i64::from(max) + 1)
                .ok()
                .and_then(|n| side(n).ok())
        };
        let [w, h, d] = bounds.max.0.to_array();
        Some(Self {
            width: cells(w)?,
            height: cells(h)?,
            depth: cells(d)?,
        })
    }
}

/// A side of `n` cells, if it is a positive multiple of a chunk that fits
/// a cell coordinate.
fn side(n: u64) -> Result<u32, String> {
    let chunk = CHUNK_SIZE as u64;
    if n == 0 || !n.is_multiple_of(chunk) || i32::try_from(n).is_err() {
        return Err(format!("{n} is not a positive multiple of {chunk}"));
    }
    Ok(n as u32)
}

impl fmt::Display for WorldSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}x{}", self.width, self.height, self.depth)
    }
}

impl FromStr for WorldSize {
    type Err = String;

    /// `WxHxD`, three decimal cell counts.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let sides: Vec<&str> = s.split('x').collect();
        let [w, h, d] = sides[..] else {
            return Err(format!("{s:?} is not WxHxD"));
        };
        let cells = |text: &str| -> Result<u32, String> {
            side(
                text.parse()
                    .map_err(|_| format!("{text:?} is not a cell count"))?,
            )
        };
        Ok(Self {
            width: cells(w)?,
            height: cells(h)?,
            depth: cells(d)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;

    use rmc_world::layers::LayerId;

    use crate::app::GameConfig;
    use crate::headless::{HeadlessReport, REPORT_FILE, run_headless};
    use crate::session::{GameAssets, assets_dir, generate_world, load_world};

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("rmc_app").chain(args.iter().copied()))
    }

    /// `args` with `--save dir` appended.
    fn parse_with_save(args: &[&str], dir: &Path) -> Cli {
        let mut all: Vec<OsString> = std::iter::once("rmc_app")
            .chain(args.iter().copied())
            .chain(["--save"])
            .map(OsString::from)
            .collect();
        all.push(dir.as_os_str().to_owned());
        Cli::try_parse_from(all).unwrap()
    }

    /// A save directory under the system temp dir, removed on drop. It is
    /// not created: an absent directory holds no save.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("rmc_app_cli_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn default_world_size_is_32_chunks() {
        // SAD-P2 §2: 128 × 64 × 128 cells, 4 × 2 × 4 chunks.
        let size = WorldSize {
            width: 128,
            height: 64,
            depth: 128,
        };
        assert_eq!(DEFAULT_WORLD_SIZE, size);
        assert_eq!(size.chunks(), 32);
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(127, 63, 127));
        assert_eq!(size.bounds(), bounds);
        assert_eq!(bounds.chunks().count(), 32);
        assert_eq!(WorldSize::of_bounds(bounds), Some(size));
        assert_eq!(size.to_string(), "128x64x128");
        assert_eq!("128x64x128".parse(), Ok(size));

        // No flag and no save: the default world, generated whole.
        let dir = TempDir::new("default_size");
        let cli = parse_with_save(&[], &dir.0);
        assert_eq!(cli.world_size, None);
        let config = GameConfig::from_cli(&cli).unwrap();
        assert_eq!(config.assets.worldgen.bounds.aabb(), bounds);
        let world = generate_world(&config.assets, 1).unwrap();
        assert_eq!(world.chunk_count(), 32);
        assert_eq!(world.bounds(), bounds);
        // Ground under sky: the surface lies inside the world, below its
        // top, and the temperature layer is written.
        let surface = config.assets.worldgen.surface.clone();
        assert!(surface.base_height + surface.amplitude < 63, "{surface:?}");
        let middle = world.surface_y(64, 64);
        assert!((1..63).contains(&middle), "surface at {middle}");
        assert!(!world.is_solid(CellPos::new(64, 63, 64)));
        assert!(world.is_solid(CellPos::new(64, 1, 64)));
        let chunk = world.chunk(CellPos::new(64, 0, 64).chunk()).unwrap();
        assert!(chunk.layers.data(LayerId::Temperature).is_some());

        // The flag: the Phase 1 world (256 chunks) is the shipped
        // worldgen.ron unchanged.
        let phase1_size = parse(&["--world-size", "256x128x256"])
            .unwrap()
            .world_size
            .unwrap();
        assert_eq!(phase1_size.chunks(), 256);
        assert_eq!(phase1_size.bounds(), CellAabb::PHASE1_DEFAULT);
        let shipped = GameAssets::load(&assets_dir()).unwrap().worldgen;
        let phase1 = parse_with_save(&["--world-size", "256x128x256"], &dir.0);
        assert_eq!(
            GameConfig::from_cli(&phase1).unwrap().assets.worldgen,
            shipped
        );
        // Too low for the surface's relief.
        let low = parse_with_save(&["--world-size", "64x32x64"], &dir.0);
        assert!(GameConfig::from_cli(&low).is_err());
        // Refused: not WxHxD, not positive multiples of 32, too large.
        for bad in [
            "",
            "128x64",
            "128x64x128x32",
            "128X64X128",
            "100x64x128",
            "0x64x128",
            "-32x64x64",
            "axbxc",
            "2147483648x32x32",
        ] {
            assert!(
                bad.parse::<WorldSize>().is_err(),
                "{bad:?} should not parse"
            );
            assert!(parse(&["--world-size", bad]).is_err(), "{bad:?}");
        }
        let odd = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(99, 63, 127));
        assert_eq!(WorldSize::of_bounds(odd), None);
        let moved = CellAabb::new(CellPos::new(32, 0, 0), CellPos::new(159, 63, 127));
        assert_eq!(WorldSize::of_bounds(moved), None);
    }

    #[test]
    fn headless_seasons_writes_report() {
        let dir = TempDir::new("headless");
        let args = [
            "--headless",
            "--seasons",
            "2",
            "--seed",
            "7",
            "--world-size",
            "64x64x64",
        ];
        let cli = parse_with_save(&args, &dir.0);
        assert!(cli.headless);
        assert_eq!(cli.seasons, Some(2));
        let config = GameConfig::from_cli(&cli).unwrap();
        let assets = config.assets.clone();
        let substeps = usize::from(assets.eco.seasons().substeps);
        let mut seen = Vec::new();
        let report = run_headless(config, 2, |s| seen.push(s.season)).unwrap();
        assert_eq!(seen, [0, 1]);

        // report.json holds what the run returned.
        let text = std::fs::read_to_string(dir.0.join(REPORT_FILE)).unwrap();
        let written: HeadlessReport = serde_json::from_str(&text).unwrap();
        assert_eq!(written, report);
        assert_eq!(report.seed, 7);
        assert_eq!(report.world_size, [64, 64, 64]);
        assert_eq!(report.chunks, 8);
        assert!(!report.loaded);
        assert!(report.invariants_ok);
        assert_eq!(report.seasons.len(), 2);
        let steam = assets.eco.displacement_rules().steam;
        for (i, s) in report.seasons.iter().enumerate() {
            assert_eq!(s.season, i as u32);
            // The invariants hold: the totals after are the totals before
            // moved by the season's ledger.
            assert!(s.violations.is_empty(), "{:?}", s.violations);
            assert_eq!(s.after, s.before.expected(&s.report.ledger, steam));
            assert_eq!(s.report.substep_ms.len(), substeps);
            let jacobi = s.report.active_chunks_per_jacobi.len();
            assert!(jacobi > 0 && jacobi % substeps == 0, "{jacobi}");
            assert_eq!(s.report.between_seasons, Default::default());
        }
        assert_eq!(report.seasons[1].before, report.seasons[0].after);
        // The tick moved heat at the surface.
        assert!(report.seasons[0].changed_chunks > 0);

        // The save holds the world after the seasons.
        assert!(report.saved_chunks > 0);
        let saved = load_world(&assets, 7, &dir.0).unwrap();
        assert_eq!(saved.hash(), report.world_hash);

        // A second run continues the save, taking its seed and size.
        let again = parse_with_save(&["--headless", "--seasons", "1"], &dir.0);
        assert_eq!((again.seed, again.world_size), (None, None));
        let more = run_headless(GameConfig::from_cli(&again).unwrap(), 1, |_| {}).unwrap();
        assert!(more.loaded);
        assert_eq!((more.seed, more.world_size), (7, [64, 64, 64]));
        assert_eq!(more.seasons[0].before, report.seasons[1].after);
        assert!(more.invariants_ok);

        // --headless and --seasons come together.
        for bad in [
            &["--headless"][..],
            &["--seasons", "3"],
            &["--headless", "--seasons", "x"],
            &["--headless", "--seasons", "-1"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
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
