//! `docs/REPORT-P2.md` (F175; SAD-P2 §12, SAD §16.6): the Phase 2 report.
//!
//! `scripts/report.sh` runs the `report_p2` binary after it writes
//! `docs/REPORT.md`. [`render_report_p2`] builds the Markdown from:
//!
//! - `features.json`: the Phase 2 (F1xx) features, their milestones and
//!   status;
//! - the last `soak_seasons` record ([`SOAK_STATS_FILE`], F174): each
//!   seed's ledger since the start, its seasons and its chaos summary;
//! - the screenshots in `docs/screens/`: the layer views (overlay, tints,
//!   spoil indicator) first, then every Phase 2 scenario's shots;
//! - `docs/perf-p2.md`.
//!
//! A screenshot belongs to Phase 2 unless a Phase 1 (F0xx) feature names
//! its scenario. Phase 1 closed with every one of its scenarios named by a
//! feature; some Phase 2 look-and-feel scenarios (`gas_tint_shows`, ...)
//! are named by none.

use std::fmt::Write as _;

use anyhow::Context;
use rmc_eco::EcoTables;
use serde::Deserialize;

/// The report, relative to the repository root.
pub const REPORT_P2_FILE: &str = "docs/REPORT-P2.md";
/// Where `soak_seasons` leaves its record, relative to the repository root.
pub const SOAK_STATS_FILE: &str = "target/e2e/soak_seasons-stats.txt";
/// Phase 1 feature ids start with this; Phase 2 ids with [`PHASE_2`].
const PHASE_1: &str = "F0";
const PHASE_2: &str = "F1";
/// Words in a scenario name that mark a view of the layers.
const LAYER_VIEW_WORDS: [&str; 3] = ["overlay", "tint", "indicator"];
/// The soak record's ledger line: `seed N ledger since start SeasonLedger { .. }`.
const LEDGER_MARK: &str = "ledger since start";
/// The soak record's cycle line: `seed N cycles (first season, ms): [..]`.
const CYCLES_MARK: &str = "cycles";
/// Words in a `docs/perf-p2.md` table cell that mark a target as not met.
const PERF_SHORT_WORDS: [&str; 2] = ["missed", "not measured"];
/// Season-line keys left out of the season tables (per sub-step detail).
const SEASON_DETAIL_KEYS: [&str; 1] = ["substep_ms"];

/// What the Phase 2 report is built from.
pub struct ReportP2Inputs<'a> {
    /// When the report was generated (UTC, ISO 8601).
    pub generated: &'a str,
    /// The commit it describes.
    pub commit: &'a str,
    /// The text of `features.json`.
    pub features_json: &'a str,
    /// The PNG file names in `docs/screens/`.
    pub screens: &'a [String],
    /// The last `soak_seasons` record, if there is one.
    pub soak_stats: Option<&'a str>,
    /// The text of `docs/perf-p2.md`, if it exists.
    pub perf_p2: Option<&'a str>,
    pub names: &'a LedgerNames,
}

/// Names for the ledger's indexed entries: substance and gas rows in table
/// order, materials by id.
#[derive(Clone, Debug, Default)]
pub struct LedgerNames {
    pub substances: Vec<String>,
    pub gases: Vec<String>,
    pub materials: Vec<String>,
}

impl LedgerNames {
    pub fn from_tables(eco: &EcoTables) -> Self {
        Self {
            substances: eco.substances().iter().map(|s| s.key.clone()).collect(),
            gases: eco.gases().iter().map(|g| g.key.clone()).collect(),
            materials: eco.materials().iter().map(|m| m.key.clone()).collect(),
        }
    }

    /// The name of entry `i` of a ledger array of `len` entries. Substance
    /// arrays and gas arrays differ in length.
    fn entry(&self, len: usize, i: usize) -> String {
        let table = if len == self.substances.len() {
            &self.substances
        } else if len == self.gases.len() {
            &self.gases
        } else {
            return format!("#{i}");
        };
        table.get(i).cloned().unwrap_or_else(|| format!("#{i}"))
    }

    fn material(&self, id: i64) -> String {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.materials.get(i).cloned())
            .unwrap_or_else(|| format!("material {id}"))
    }
}

#[derive(Deserialize)]
struct FeatureFile {
    features: Vec<Feature>,
}

#[derive(Deserialize)]
struct Feature {
    id: String,
    milestone: String,
    title: String,
    #[serde(default)]
    tests: Vec<String>,
    #[serde(default)]
    passes: bool,
    #[serde(default)]
    skipped: bool,
    #[serde(default, alias = "skipReason", alias = "skippedReason")]
    skip_reason: Option<String>,
    #[serde(default)]
    notes: String,
}

impl Feature {
    fn status(&self) -> String {
        if self.passes {
            "passed".into()
        } else if self.skipped {
            let reason = self.skip_reason.as_deref().unwrap_or("see PROGRESS.md");
            format!("skipped: {}", cell(reason))
        } else {
            "**no**".into()
        }
    }

    /// Whether the feature's title, tests or notes name `scenario`.
    fn names(&self, scenario: &str) -> bool {
        mentions(&self.title, scenario)
            || mentions(&self.notes, scenario)
            || self.tests.iter().any(|t| mentions(t, scenario))
    }
}

/// Whether `text` holds `word` with no scenario-name character on either
/// side.
fn mentions(text: &str, word: &str) -> bool {
    let part = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(word).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(part)
            && !text[at + word.len()..].chars().next().is_some_and(part)
    })
}

/// `text` made safe for a Markdown table cell.
fn cell(text: &str) -> String {
    text.replace('|', "\\|")
}

/// The scenario a screenshot belongs to: `<scenario>[-<label>]-<frame>.png`.
/// Scenario names use underscores, never dashes.
fn scenario_of(screen: &str) -> &str {
    let stem = screen.strip_suffix(".png").unwrap_or(screen);
    stem.split('-').next().unwrap_or(stem)
}

/// Renders the report.
pub fn render_report_p2(inputs: &ReportP2Inputs) -> anyhow::Result<String> {
    let file: FeatureFile =
        serde_json::from_str(inputs.features_json).context("parsing features.json")?;
    let phase = |prefix: &str| -> Vec<&Feature> {
        file.features
            .iter()
            .filter(|f| f.id.starts_with(prefix))
            .collect()
    };
    let (phase1, phase2) = (phase(PHASE_1), phase(PHASE_2));
    let mut screens: Vec<&str> = inputs
        .screens
        .iter()
        .map(String::as_str)
        .filter(|s| !phase1.iter().any(|f| f.names(scenario_of(s))))
        .collect();
    screens.sort_unstable();

    let mut out = String::new();
    header(&mut out, inputs, &phase2)?;
    milestones(&mut out, &phase2)?;
    feature_status(&mut out, &phase2)?;
    soak(&mut out, inputs.soak_stats, inputs.names)?;
    layer_views(&mut out, &screens, &phase2)?;
    gallery(&mut out, &screens, &phase2)?;
    match inputs.perf_p2 {
        // perf-p2.md's own title becomes a section; its subsections nest under it.
        Some(perf) => {
            for line in perf.lines() {
                let deeper = if line.starts_with('#') { "#" } else { "" };
                writeln!(out, "{deeper}{line}")?;
            }
        }
        None => writeln!(
            out,
            "## Phase 2 performance\n\n(docs/perf-p2.md not written)"
        )?,
    }
    Ok(out)
}

fn header(out: &mut String, inputs: &ReportP2Inputs, phase2: &[&Feature]) -> anyhow::Result<()> {
    let passed = phase2.iter().filter(|f| f.passes).count();
    let skipped = phase2.iter().filter(|f| !f.passes && f.skipped).count();
    writeln!(out, "# Rust Mining Co — Phase 2 report (ecology)\n")?;
    writeln!(
        out,
        "Generated {} at commit {}. Design: [docs/SAD-P2.md](SAD-P2.md). \
         The whole build, with every decision and blocker: [docs/REPORT.md](REPORT.md). \
         Playtester reports: [docs/PLAYTEST.md](PLAYTEST.md).\n",
        inputs.generated, inputs.commit
    )?;
    writeln!(
        out,
        "**{passed} / {} Phase 2 features passing, {skipped} skipped, {} not passing.**\n",
        phase2.len(),
        phase2.len() - passed - skipped
    )?;
    let short = inputs.perf_p2.map(perf_shortfalls).unwrap_or_default();
    if !short.is_empty() {
        writeln!(
            out,
            "**SAD-P2 §13 performance targets missed or not measured** (details under \
             Phase 2 performance below):\n"
        )?;
        for (metric, status) in short {
            writeln!(out, "- {metric}: {status}")?;
        }
        writeln!(out)?;
    }
    writeln!(out, "## How to watch the seasons\n")?;
    writeln!(out, "```")?;
    writeln!(out, "cargo run --release -p rmc_app -- --seed 42")?;
    writeln!(
        out,
        "cargo run --release -p rmc_app -- --headless --seasons 4 --save season-run"
    )?;
    writeln!(out, "```\n")?;
    writeln!(
        out,
        "In the game, `F6` advances one season and `F7` steps through the layer overlays. \
         The editor's eco panel shows the last season's report; see \
         [docs/CONTROLS.md](CONTROLS.md). The headless run writes `report.json` (every \
         season's ledger and invariant check) into its save directory.\n"
    )?;
    Ok(())
}

/// The metrics whose current status in `docs/perf-p2.md` is missed or not
/// measured, with that status cell. The doc lists the newest results first,
/// so a metric's first table row is its current status; later rows for the
/// same metric are older baselines.
fn perf_shortfalls(perf: &str) -> Vec<(String, String)> {
    let mut seen: Vec<String> = Vec::new();
    let mut short = Vec::new();
    for line in perf.lines().filter(|l| l.starts_with('|')) {
        let cells: Vec<String> = split_top(line.trim_matches('|'), '|')
            .into_iter()
            .map(|c| c.replace("**", ""))
            .collect();
        let Some(metric) = cells.first() else {
            continue;
        };
        if metric.starts_with("---") || seen.contains(metric) {
            continue;
        }
        seen.push(metric.clone());
        let status = cells.iter().skip(1).find(|c| {
            let c = c.to_lowercase();
            PERF_SHORT_WORDS.iter().any(|w| c.contains(w))
        });
        if let Some(status) = status {
            short.push((metric.clone(), status.clone()));
        }
    }
    short
}

fn milestones(out: &mut String, phase2: &[&Feature]) -> anyhow::Result<()> {
    writeln!(out, "## Milestones\n")?;
    writeln!(out, "| milestone | passing | skipped | not passing |")?;
    writeln!(out, "|---|---|---|---|")?;
    let mut order: Vec<&str> = Vec::new();
    for f in phase2 {
        if !order.contains(&f.milestone.as_str()) {
            order.push(&f.milestone);
        }
    }
    for m in order {
        let group: Vec<&&Feature> = phase2.iter().filter(|f| f.milestone == m).collect();
        let passed = group.iter().filter(|f| f.passes).count();
        let skipped = group.iter().filter(|f| !f.passes && f.skipped).count();
        let open: Vec<&str> = group
            .iter()
            .filter(|f| !f.passes && !f.skipped)
            .map(|f| f.id.as_str())
            .collect();
        writeln!(
            out,
            "| {m} | {passed} / {} | {skipped} | {} |",
            group.len(),
            open.join(", ")
        )?;
    }
    writeln!(out)?;
    Ok(())
}

fn feature_status(out: &mut String, phase2: &[&Feature]) -> anyhow::Result<()> {
    writeln!(out, "## Feature status\n")?;
    writeln!(out, "| id | milestone | title | status | tests |")?;
    writeln!(out, "|---|---|---|---|---|")?;
    for f in phase2 {
        let tests: Vec<String> = f.tests.iter().map(|t| format!("`{}`", cell(t))).collect();
        writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            f.id,
            f.milestone,
            cell(&f.title),
            f.status(),
            tests.join("<br>")
        )?;
    }
    writeln!(out)?;
    Ok(())
}

/// One seed of the soak record.
#[derive(Default)]
struct SoakSeed {
    seed: String,
    /// What ChaosBot did.
    chaos: Option<String>,
    /// Each season's `key value` pairs, in order.
    seasons: Vec<Vec<(String, String)>>,
    /// The ledger since the start: `field: value` pairs, in order.
    ledger: Vec<(String, String)>,
    cycles: Option<String>,
}

/// Parses the record `soak_seasons` writes (see its module docs). Lines it
/// does not know are skipped.
fn parse_soak(text: &str) -> Vec<SoakSeed> {
    let mut seeds: Vec<SoakSeed> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("seed ") else {
            continue;
        };
        let (seed, rest) = rest.split_at(rest.find([' ', ':']).unwrap_or(rest.len()));
        if seeds.last().is_none_or(|s| s.seed != seed) {
            seeds.push(SoakSeed {
                seed: seed.to_string(),
                ..SoakSeed::default()
            });
        }
        let entry = seeds.last_mut().expect("pushed above");
        let words = rest.trim_start();
        if let Some(chaos) = rest.strip_prefix(':') {
            entry.chaos = Some(chaos.trim().to_string());
        } else if let Some(fields) = words.strip_prefix(LEDGER_MARK) {
            let body = fields.trim();
            let body = body.find('{').map_or(body, |at| &body[at + 1..]);
            entry.ledger = split_top(body.trim_end().trim_end_matches('}'), ',')
                .into_iter()
                .filter_map(|f| {
                    let (k, v) = f.split_once(':')?;
                    Some((k.trim().to_string(), v.trim().to_string()))
                })
                .collect();
        } else if let Some(cycles) = words.strip_prefix(CYCLES_MARK) {
            entry.cycles = Some(cycles.trim().to_string());
        } else if words.starts_with("season ") {
            let pairs = split_top(words, ' ')
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[k, v]| (k.to_string(), v.to_string()))
                .collect();
            entry.seasons.push(pairs);
        }
    }
    seeds
}

/// Splits `text` at `sep` outside brackets, braces and parentheses, and
/// drops empty parts.
fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in text.char_indices() {
        match c {
            '[' | '(' | '{' => depth += 1,
            ']' | ')' | '}' => depth -= 1,
            _ if c == sep && depth == 0 => {
                parts.push(text[start..i].trim());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts.retain(|p| !p.is_empty());
    parts
}

/// The integers in `text`, in order.
fn integers(text: &str) -> Vec<i64> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '-'))
        .filter_map(|w| w.parse().ok())
        .collect()
}

/// A ledger value for the table: arrays list their non-zero entries by
/// name, transitions read `from → to count`.
fn ledger_value(value: &str, names: &LedgerNames) -> String {
    let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) else {
        return cell(value);
    };
    let items = split_top(inner, ',');
    if items.iter().any(|i| i.starts_with('(')) {
        let moves: Vec<String> = items
            .iter()
            .filter_map(|t| match integers(t)[..] {
                [from, to, count] => Some(format!(
                    "{} → {} {count}",
                    names.material(from),
                    names.material(to)
                )),
                _ => None,
            })
            .collect();
        return moves.join(", ");
    }
    if items.is_empty() {
        return "none".into();
    }
    let entries: Vec<String> = items
        .iter()
        .enumerate()
        .filter(|(_, v)| v.parse::<i64>() != Ok(0))
        .map(|(i, v)| format!("{} {}", names.entry(items.len(), i), cell(v)))
        .collect();
    if entries.is_empty() {
        "0".into()
    } else {
        entries.join(", ")
    }
}

/// The ledger fields of every seed, in first-seen order.
fn ledger_fields(seeds: &[SoakSeed]) -> Vec<&str> {
    let mut fields: Vec<&str> = Vec::new();
    for (k, _) in seeds.iter().flat_map(|s| &s.ledger) {
        if !fields.contains(&k.as_str()) {
            fields.push(k);
        }
    }
    fields
}

fn soak(out: &mut String, stats: Option<&str>, names: &LedgerNames) -> anyhow::Result<()> {
    writeln!(out, "## Soak: ledgers of the last soak_seasons run\n")?;
    let seeds = stats.map(parse_soak).unwrap_or_default();
    if seeds.is_empty() {
        writeln!(
            out,
            "No soak_seasons record in `{SOAK_STATS_FILE}`. Run `scripts/e2e.sh soak_seasons` \
             (about 30 minutes) and regenerate the report.\n"
        )?;
        return Ok(());
    }
    writeln!(
        out,
        "From `{SOAK_STATS_FILE}`, which the scenario writes only after all of its seeds pass (the file is not committed; this table is its copy). Each seed runs ChaosBot for 600 frames, then 20 seasons on the default world. After every season it checks every conservation invariant against the ledger and the replica hash against the host's (F174).\n"
    )?;
    writeln!(
        out,
        "The ledger holds everything the world gained and lost since before the soak's fixture was laid. Units: the water terms (`rain_in` to `condense_from_steam`) are moisture units, 255 to a full cell; the substance arrays are layer units; the gas arrays are concentration units; the magma terms are cells; the rest are counts. Zero entries of the per-substance and per-gas arrays are left out.\n"
    )?;
    let idle: Vec<&str> = ledger_fields(&seeds)
        .into_iter()
        .filter(|field| {
            seeds.iter().all(|s| {
                s.ledger
                    .iter()
                    .find(|(k, _)| k == field)
                    .is_none_or(|(_, v)| integers(v).iter().all(|n| *n == 0))
            })
        })
        .collect();
    if !idle.is_empty() {
        let idle: Vec<String> = idle.iter().map(|f| format!("`{f}`")).collect();
        writeln!(
            out,
            "Not exercised by this run (zero in every seed), so the invariants behind them were not tested here: {}.\n",
            idle.join(", ")
        )?;
    }
    let header: Vec<String> = seeds.iter().map(|s| format!("seed {}", s.seed)).collect();
    writeln!(out, "| field | {} |", header.join(" | "))?;
    writeln!(out, "|---|{}", "---|".repeat(seeds.len()))?;
    for field in ledger_fields(&seeds) {
        let values: Vec<String> = seeds
            .iter()
            .map(|s| {
                s.ledger
                    .iter()
                    .find(|(k, _)| k == field)
                    .map_or_else(String::new, |(_, v)| ledger_value(v, names))
            })
            .collect();
        writeln!(out, "| `{field}` | {} |", values.join(" | "))?;
    }
    writeln!(out)?;
    for s in &seeds {
        writeln!(out, "### Seed {}\n", s.seed)?;
        if let Some(chaos) = &s.chaos {
            writeln!(out, "Chaos: {chaos}.\n")?;
        }
        if let Some(cycles) = &s.cycles {
            writeln!(out, "Cycle sums {cycles}\n")?;
        }
        let Some(first) = s.seasons.first() else {
            continue;
        };
        writeln!(
            out,
            "Per season: `ms` is the season's wall-clock time in the dev profile on the test kit's host (not the release benches under performance); the other columns are that season's counts and ledger terms.\n"
        )?;
        let keys: Vec<&str> = first
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| !SEASON_DETAIL_KEYS.contains(k))
            .collect();
        writeln!(out, "| {} |", keys.join(" | "))?;
        writeln!(out, "|{}", "---|".repeat(keys.len()))?;
        for season in &s.seasons {
            let row: Vec<&str> = keys
                .iter()
                .map(|k| {
                    season
                        .iter()
                        .find(|(key, _)| key == k)
                        .map_or("", |(_, v)| v.as_str())
                })
                .collect();
            writeln!(out, "| {} |", row.join(" | "))?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn is_layer_view(scenario: &str) -> bool {
    LAYER_VIEW_WORDS.iter().any(|w| scenario.contains(w))
}

fn layer_views(out: &mut String, screens: &[&str], phase2: &[&Feature]) -> anyhow::Result<()> {
    writeln!(out, "## Layer views: overlay, tints, indicators\n")?;
    let views: Vec<&str> = screens
        .iter()
        .copied()
        .filter(|s| is_layer_view(scenario_of(s)))
        .collect();
    if views.is_empty() {
        writeln!(out, "(none in docs/screens)\n")?;
    }
    scenario_blocks(out, &views, phase2)
}

fn gallery(out: &mut String, screens: &[&str], phase2: &[&Feature]) -> anyhow::Result<()> {
    writeln!(out, "## Screenshots (Phase 2 scenarios)\n")?;
    writeln!(
        out,
        "Every other Phase 2 scenario's committed shots (the layer views are above).\n"
    )?;
    let rest: Vec<&str> = screens
        .iter()
        .copied()
        .filter(|s| !is_layer_view(scenario_of(s)))
        .collect();
    scenario_blocks(out, &rest, phase2)
}

/// One subsection per scenario of `screens` (sorted): the features that
/// name it, then its shots.
fn scenario_blocks(out: &mut String, screens: &[&str], phase2: &[&Feature]) -> anyhow::Result<()> {
    let mut scenarios: Vec<&str> = screens.iter().map(|s| scenario_of(s)).collect();
    scenarios.dedup();
    for scenario in scenarios {
        writeln!(out, "### `{scenario}`\n")?;
        let owners: Vec<String> = phase2
            .iter()
            .filter(|f| f.names(scenario))
            .map(|f| format!("{} ({}, {})", f.id, f.milestone, f.status()))
            .collect();
        if owners.is_empty() {
            writeln!(
                out,
                "No feature lists this scenario: it is the screenshot check of a look-and-feel feature whose own tests are unit tests (see PROGRESS.md).\n"
            )?;
        } else {
            writeln!(out, "Features: {}.\n", owners.join(", "))?;
        }
        for s in screens.iter().filter(|s| scenario_of(s) == scenario) {
            writeln!(out, "![{s}](screens/{s})")?;
        }
        writeln!(out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEATURES: &str = r#"{ "features": [
        { "id": "F056", "milestone": "M5", "title": "Testkit: boot_renders",
          "tests": ["e2e:boot_renders"], "passes": true },
        { "id": "F162", "milestone": "E4", "title": "E2E: layer_overlay_renders",
          "tests": ["e2e:layer_overlay_renders"], "passes": true },
        { "id": "F174", "milestone": "E5", "title": "E2E: soak_seasons | three seeds",
          "tests": ["e2e:soak_seasons"], "passes": true },
        { "id": "F175", "milestone": "E5", "title": "Playtester report",
          "tests": ["cargo:rmc_app:report::tests::report_p2_includes_ledgers_and_screens"],
          "passes": false }
    ] }"#;

    const SOAK: &str = "\
seed 1: 13 actions (Walk 3, Dig 3), 8 swings
seed 1 season 0 ms 12566 substep_ms [690, 1567] active_cells 11000768 rain_in 32782543
seed 1 season 1 ms 14648 substep_ms [1994, 1849] active_cells 14494514 rain_in 16232872
seed 1 ledger since start SeasonLedger { rain_in: 270389391, steam_vent_out: 4809781, \
weather_in: [3248, 2077, 0], uptake_sink: [0, 0, 0], emit_in: [0, 1039746], \
transitions: [(MaterialId(1), MaterialId(2), 1132), (MaterialId(2), MaterialId(1), 7)], \
plants_died: 63 }
seed 1 cycles (first season, ms): [(4, 70019), (8, 73471)]
seed 2: 14 actions (Walk 2), 0 swings
seed 2 season 0 ms 12714 substep_ms [698, 1562] active_cells 11248443 rain_in 32982986
seed 2 ledger since start SeasonLedger { rain_in: 270304298, steam_vent_out: 4696160, \
weather_in: [3132, 0, 4266], uptake_sink: [0, 0, 0], emit_in: [5, 0], \
transitions: [], plants_died: 61 }
";

    /// Newest results first: `tick_first` was missed before and is met now.
    const PERF: &str = "# Phase 2 performance (SAD-P2 §13)

## Bench results

| Bench | Time | §13 target | Status |
|---|---|---|---|
| `tick_first` | 3.03 s | < 5 s | met |
| `tick_quiet` | 751 ms | < 1 s | met once settled |

| Bench | Time | §13 target | Status |
|---|---|---|---|
| `tick_first` | 10.9 s | < 5 s | missed |

| Metric | Target | Where it stands |
|---|---|---|
| Steady rain, 50 edits | < 3 s | **Missed**: about 3.6–4.4 s |
| Snapshots + remesh | < 1.5 s | Not measured (host and client side). |
";

    fn names() -> LedgerNames {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        LedgerNames {
            substances: s(&["cu", "fe", "acid"]),
            gases: s(&["steam", "firedamp"]),
            materials: s(&["air", "water", "ice"]),
        }
    }

    fn screens() -> Vec<String> {
        [
            "boot_renders-123.png",
            "gas_tint_shows-steam-47.png",
            "layer_overlay_renders-27.png",
            "layer_overlay_renders-material-14.png",
            "soak_seasons-s1-seasons-780.png",
        ]
        .map(String::from)
        .to_vec()
    }

    fn render(soak: Option<&str>) -> String {
        let names = names();
        let screens = screens();
        render_report_p2(&ReportP2Inputs {
            generated: "2026-10-23T05:00:00Z",
            commit: "abc1234",
            features_json: FEATURES,
            screens: &screens,
            soak_stats: soak,
            perf_p2: Some(PERF),
            names: &names,
        })
        .expect("the report renders")
    }

    /// The section of `report` that starts at the heading `title`, up to
    /// the next heading of the same level.
    fn section<'r>(report: &'r str, title: &str) -> &'r str {
        let start = report
            .find(&format!("\n## {title}"))
            .unwrap_or_else(|| panic!("no section {title:?} in:\n{report}"))
            + 1;
        let rest = &report[start..];
        let end = rest[3..].find("\n## ").map_or(rest.len(), |e| e + 4);
        &rest[..end]
    }

    #[test]
    fn report_p2_includes_ledgers_and_screens() {
        let report = render(Some(SOAK));
        assert!(report.starts_with("# "), "{report}");
        assert!(report.contains("abc1234") && report.contains("2026-10-23T05:00:00Z"));
        // Phase 2 features only, with their status; a `|` in a title is escaped.
        assert!(
            report.contains("**2 / 3 Phase 2 features passing"),
            "{report}"
        );
        let status = section(&report, "Feature status");
        assert!(
            status.contains("| F175 | E5 | Playtester report | **no** |"),
            "{status}"
        );
        assert!(status.contains("soak_seasons \\| three seeds"), "{status}");
        assert!(!status.contains("F056"), "{status}");

        // The last soak's ledger since the start, one column per seed, with
        // indexed entries named and zero entries left out.
        let soak = section(&report, "Soak");
        assert!(soak.contains("| seed 1 | seed 2 |"), "{soak}");
        assert!(
            soak.contains("| `rain_in` | 270389391 | 270304298 |"),
            "{soak}"
        );
        assert!(
            soak.contains("| `weather_in` | cu 3248, fe 2077 | cu 3132, acid 4266 |"),
            "{soak}"
        );
        assert!(soak.contains("| `uptake_sink` | 0 | 0 |"), "{soak}");
        assert!(
            soak.contains("| `emit_in` | firedamp 1039746 | steam 5 |"),
            "{soak}"
        );
        assert!(soak.contains("water → ice 1132, ice → water 7"), "{soak}");
        assert!(soak.contains("| `plants_died` | 63 | 61 |"), "{soak}");
        // Per-season rows, and each seed's chaos summary and cycle sums.
        assert!(
            soak.contains("| 1 | 14648 | 14494514 | 16232872 |"),
            "{soak}"
        );
        assert!(
            soak.contains("13 actions (Walk 3, Dig 3), 8 swings"),
            "{soak}"
        );
        assert!(soak.contains("[(4, 70019), (8, 73471)]"), "{soak}");

        // Units, and the ledger terms the run never moved.
        assert!(
            soak.contains("moisture units, 255 to a full cell"),
            "{soak}"
        );
        assert!(soak.contains("Not exercised by this run"), "{soak}");
        assert!(
            soak.contains("`uptake_sink`") && !soak.contains("`rain_in`, "),
            "{soak}"
        );

        // The layer views get their own section, the gallery holds every
        // other Phase 2 screenshot, each shown once, and no Phase 1 shot.
        let views = section(&report, "Layer views");
        let gallery = section(&report, "Screenshots");
        for shot in &screens()[1..] {
            let image = format!("![{shot}](screens/{shot})");
            let in_views = views.contains(&image);
            assert_eq!(
                in_views,
                !shot.starts_with("soak_seasons"),
                "{shot}: {views}"
            );
            assert_eq!(gallery.contains(&image), !in_views, "{shot}: {gallery}");
            assert_eq!(report.matches(&image).count(), 1, "{shot}");
        }
        assert!(views.contains("F162 (E4, passed)"), "{views}");
        assert!(views.contains("No feature lists this scenario"), "{views}");
        assert!(gallery.contains("F174 (E5, passed)"), "{gallery}");
        assert!(!report.contains("boot_renders"), "{report}");

        // Targets currently missed or not measured are in the headline;
        // an old baseline row that was missed but is met now is not.
        let head = &report[..report
            .find(
                "
## ",
            )
            .expect("sections")];
        assert!(
            head.contains("- Steady rain, 50 edits: Missed: about 3.6–4.4 s"),
            "{head}"
        );
        assert!(
            head.contains("- Snapshots + remesh: Not measured"),
            "{head}"
        );
        assert!(!head.contains("tick_first"), "{head}");

        // The Phase 2 performance doc, one heading level down.
        assert!(
            report.contains("\n## Phase 2 performance (SAD-P2 §13)\n"),
            "{report}"
        );
        assert!(report.contains("\n### Bench results\n"), "{report}");

        // Without a soak record the section says so.
        let none = render(None);
        assert!(
            section(&none, "Soak").contains("No soak_seasons record"),
            "{none}"
        );
    }
}
