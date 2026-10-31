//! `--tree-lab` (docs/TREES.md T4): grows one tree headless with
//! [`rmc_eco::tree_lab`] and writes it after every flush as JSON for the
//! browser viewer.
//!
//! The file holds the species' look (bark and leaf colors, the season
//! colors, the Graph form) and one frame per flush: its season and every
//! metamer with its parent, direction (Q14), length, birth season, leaf,
//! end point (mm from the root's top face) and pipe radius (µm).

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use rmc_eco::EcoTables;
use rmc_eco::tables::{EcoSources, FormDef};
use rmc_eco::tree_lab::{self, LabFrame};
use rmc_world::materials::MaterialTable;
use serde_json::{Value, json};

/// What a lab run wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabSummary {
    pub frames: usize,
    /// Metamers in the last frame.
    pub metamers: usize,
}

/// Grows `species` for `years` cycles of `seasons.ron` with `flushes`
/// flushes a season and writes the JSON to `out`.
pub fn run_tree_lab(
    assets: &Path,
    species: &str,
    seed: u64,
    years: u32,
    flushes: u32,
    out: &Path,
) -> anyhow::Result<LabSummary> {
    let materials = Arc::new(MaterialTable::load(assets.join("materials.ron"))?);
    let src = tree_lab::with_forms(&EcoSources::read(assets)?, &tree_lab::read_forms(assets)?)?;
    let tables = EcoTables::from_sources(materials, &src)?;
    let seasons = years * tables.seasons().seasons.len() as u32;
    let frames = tree_lab::grow(&tables, species, seed, seasons, flushes)?;
    let doc = lab_json(&tables, species, seed, &frames)?;
    std::fs::write(out, serde_json::to_string(&doc)?)
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(LabSummary {
        frames: frames.len(),
        metamers: frames.last().map_or(0, |f| f.graph.len()),
    })
}

/// The lab file for `frames` of `species`.
pub fn lab_json(
    tables: &EcoTables,
    species: &str,
    seed: u64,
    frames: &[LabFrame],
) -> anyhow::Result<Value> {
    let row = tables
        .plants()
        .iter()
        .find(|p| p.key == species)
        .with_context(|| format!("no species {species:?}"))?;
    let parts = row.parts.as_ref().context("no parts")?;
    let FormDef::Graph {
        internode_mm,
        tip_um,
        ring_um,
        divergence,
        leaf,
        ..
    } = parts.form
    else {
        anyhow::bail!("{species:?} does not grow as a graph");
    };
    let color = |key: &str| {
        let m = tables.materials();
        m.id(key)
            .and_then(|id| m.get(id))
            .map(|d| [d.color[0], d.color[1], d.color[2]])
    };
    let frames: Vec<Value> = frames
        .iter()
        .map(|f| {
            let ends = f.graph.ends();
            let radii = f.graph.wood_radii(tip_um, ring_um);
            let metamers: Vec<Value> = f
                .graph
                .metamers()
                .iter()
                .zip(ends.iter().zip(&radii))
                .map(|(m, (end, r))| {
                    json!({
                        "parent": m.parent,
                        "dir": m.dir,
                        "length_mm": m.length_mm,
                        "born": m.born,
                        "leaf": m.leaf,
                        "bud": m.bud.byte(),
                        "lateral": m.lateral,
                        "end": end,
                        "radius_um": r,
                    })
                })
                .collect();
            json!({
                "season": f.season,
                "season_key": f.season_key,
                "flush": f.flush,
                "metamers": metamers,
            })
        })
        .collect();
    Ok(json!({
        "species": species,
        "seed": seed,
        "internode_mm": internode_mm,
        "tip_um": tip_um,
        "divergence": divergence,
        "leaf": leaf,
        "shed": parts.shed,
        "bark": color(&parts.wood),
        "leaf_color": color(&parts.leaf),
        "season_colors": row.season_colors,
        "frames": frames,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::assets_dir;

    #[test]
    fn the_lab_writes_a_frame_per_flush_with_ends_and_radii() {
        let out = std::env::temp_dir().join(format!("rmc_tree_lab_{}.json", std::process::id()));
        let summary = run_tree_lab(&assets_dir(), "oak", 7, 1, 2, &out).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let _ = std::fs::remove_file(&out);
        let frames = doc["frames"].as_array().unwrap();
        assert_eq!(frames.len(), summary.frames);
        assert!(summary.metamers > 0);
        let last = frames.last().unwrap()["metamers"].as_array().unwrap();
        assert_eq!(last.len(), summary.metamers);
        // The first metamer starts at the root and has the thickest pipe.
        let r = |m: &Value| m["radius_um"].as_u64().unwrap();
        assert!(last.iter().all(|m| r(m) <= r(&last[0])));
        assert_eq!(last[0]["parent"], u16::MAX);
        assert!(doc["bark"].is_array() && doc["leaf_color"].is_array());
    }
}
