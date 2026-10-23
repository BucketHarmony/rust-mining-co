//! Writes `docs/REPORT-P2.md` (see `rmc_app::report`). Run from the
//! repository root by `scripts/report.sh`; opens no window.
#![deny(unsafe_code)]

use std::path::Path;

use anyhow::Context;
use clap::Parser;
use rmc_app::report::{
    LedgerNames, REPORT_P2_FILE, ReportP2Inputs, SOAK_STATS_FILE, render_report_p2,
};
use rmc_eco::EcoTables;

const FEATURES_FILE: &str = "features.json";
const SCREENS_DIR: &str = "docs/screens";
const PERF_P2_FILE: &str = "docs/perf-p2.md";
const ASSETS_DIR: &str = "assets";

/// `report_p2 --generated <UTC time> --commit <hash>`.
#[derive(Parser)]
struct Args {
    /// When the report is generated (UTC, ISO 8601).
    #[arg(long)]
    generated: String,
    /// The commit the report describes.
    #[arg(long)]
    commit: String,
}

/// The file's text, or `None` if it does not exist.
fn read_optional(path: &str) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {path}")),
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let features_json = std::fs::read_to_string(FEATURES_FILE)
        .with_context(|| format!("reading {FEATURES_FILE}"))?;
    let mut screens = Vec::new();
    for entry in std::fs::read_dir(SCREENS_DIR).with_context(|| format!("listing {SCREENS_DIR}"))? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if Path::new(&name).extension().is_some_and(|e| e == "png") {
            screens.push(name);
        }
    }
    let eco = EcoTables::load(ASSETS_DIR).context("loading the eco tables")?;
    let names = LedgerNames::from_tables(&eco);
    let soak_stats = read_optional(SOAK_STATS_FILE)?;
    let perf_p2 = read_optional(PERF_P2_FILE)?;
    let report = render_report_p2(&ReportP2Inputs {
        generated: &args.generated,
        commit: &args.commit,
        features_json: &features_json,
        screens: &screens,
        soak_stats: soak_stats.as_deref(),
        perf_p2: perf_p2.as_deref(),
        names: &names,
    })?;
    std::fs::write(REPORT_P2_FILE, report).with_context(|| format!("writing {REPORT_P2_FILE}"))?;
    println!("wrote {REPORT_P2_FILE}");
    Ok(())
}
