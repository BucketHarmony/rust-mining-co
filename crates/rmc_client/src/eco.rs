//! What the client reads of the ecology tables (SAD-P2 §10), the
//! targeted-cell readout the editor shows, and the season report summary
//! ([`SeasonSummary`], [`season_lines`]) of its eco panel.
//!
//! `rmc_client` does not depend on `rmc_eco`, so whoever builds the app
//! (the game, the test kit) inserts [`ClientEco`] from the loaded tables.
//! Without it the HUD shows no spoil indicator and the readout names
//! substances, gases and plant species by index and leaves out pH and
//! spoil.
//!
//! Everything here reads the replica's layers: they follow
//! `ChunkSnapshot`, `LayerSnapshot` and `ChunkDelta::Layers`, so the
//! readings are the host's as of the last season or edit.

use bevy::prelude::*;
use rmc_protocol::SeasonReport;
use rmc_world::coords::CellPos;
use rmc_world::layers::{LayerId, N_GASES, N_SUBSTANCES, SubstanceLayers};
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::{MaterialId, MaterialTable};
use rmc_world::query::{PhRule, SubstanceTint, ToxProfile};
use rmc_world::world::World;

use crate::hud::format_temperature;

/// The parts of the eco tables the client reads.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct ClientEco {
    /// Substance keys by layer.
    pub substances: SubstanceLayers,
    /// Gas keys by layer index (`gases.ron` row order).
    pub gases: Vec<String>,
    /// What pH reads.
    pub ph: PhRule,
    /// `tox_player`: the HUD's spoil indicator.
    pub player_tox: ToxProfile,
    /// `tox_plant`.
    pub plant_tox: ToxProfile,
    /// `eco.tint`: how dissolved substances shift the color of water in
    /// the liquid pass, in row order.
    pub tint: Vec<SubstanceTint>,
    /// `eco.gas_tint`: how visible gases at the camera's cell tint the
    /// view, in row order (layers are `LayerId::Gas`).
    pub gas_tint: Vec<SubstanceTint>,
    /// The flammable gases' layers and `ignite_min`: what the HUD's
    /// lantern icon reads.
    pub flammable: Vec<(LayerId, u8)>,
    /// Plant species keys by row (`plants.ron` order): what
    /// [`PlantState::species`] indexes.
    pub plants: Vec<String>,
}

impl ClientEco {
    /// The key of substance or gas layer `id`, or `#<index>` when the
    /// tables do not name it.
    pub fn layer_name(&self, id: LayerId) -> String {
        let named = match id {
            LayerId::Substance(_) => self.substances.key(id),
            LayerId::Gas(i) => self.gases.get(usize::from(i)).map(String::as_str),
            _ => None,
        };
        named.map_or_else(|| unnamed(id), str::to_string)
    }

    /// The key of plant species `species`, or `#<index>` when the tables
    /// do not name it.
    pub fn species_name(&self, species: u8) -> String {
        self.plants
            .get(usize::from(species))
            .cloned()
            .unwrap_or_else(|| format!("#{species}"))
    }
}

pub(crate) fn unnamed(id: LayerId) -> String {
    match id {
        LayerId::Substance(i) | LayerId::Gas(i) => format!("#{i}"),
        other => format!("{other:?}"),
    }
}

/// What the replica holds at one cell (SAD-P2 §10 editor readout).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellReadout {
    pub cell: CellPos,
    pub material: MaterialId,
    /// 0.1 °C.
    pub temperature: i16,
    pub moisture: u8,
    pub regolith: u8,
    pub light: u8,
    /// Derived pH; `None` without [`ClientEco`].
    pub ph: Option<i32>,
    /// Nonzero substance layers, in layer order.
    pub substances: Vec<(LayerId, u8)>,
    /// Nonzero gas layers, in layer order.
    pub gases: Vec<(LayerId, u8)>,
    /// Substances at or above the player's limit.
    pub spoil_player: Vec<LayerId>,
    /// Substances at or above the plants' limit.
    pub spoil_plant: Vec<LayerId>,
    /// The plant growing in the cell, if it holds a plant state.
    pub plant: Option<PlantReadout>,
}

/// A plant's state as the readout shows it (SAD-P2 §10: species, stage,
/// held).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantReadout {
    /// Row in `plants.ron`.
    pub species: u8,
    /// The cell's `growth` layer: the plant's stage. 255 is mature, the
    /// stage at which it spreads (SAD-P2 §7.8).
    pub growth: u8,
    pub age_seasons: u8,
    pub stress: u8,
    /// Nonzero held substances, in layer order.
    pub held: Vec<(LayerId, u8)>,
}

/// What the client last heard of season ticks (SAD-P2 §9 `SeasonStarted`,
/// `SeasonDone`): the editor panel's season report summary.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct SeasonSummary {
    /// A season that started and has not finished.
    pub running: Option<u32>,
    /// The last season that finished.
    pub last: Option<SeasonDone>,
}

/// One finished season, as `SeasonDone` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeasonDone {
    pub season: u32,
    pub changed_chunks: u32,
    pub report: SeasonReport,
}

/// The season report summary as the editor panel shows it.
///
/// - A season that is running, then the last finished one: its number,
///   changed chunks and time over all sub-steps.
/// - Water: rain in, vented as steam and lost to saturation. Steam's
///   moves are water moves, so they are counted here and not as a gas.
/// - The net change of each substance and gas the season moved, from the
///   SAD-P2 §7 conservation terms, and the plant and transition counts.
/// - The same water, substance and gas nets for the edits made since the
///   season before it (`between_seasons`).
pub fn season_lines(summary: &SeasonSummary, eco: Option<&ClientEco>) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(season) = summary.running {
        lines.push(format!("Season {season} running"));
    }
    let Some(done) = &summary.last else {
        lines.push("No season yet".to_string());
        return lines;
    };
    let (l, e) = (&done.report.ledger, &done.report.between_seasons);
    let ms: u64 = done.report.substep_ms.iter().map(|&m| u64::from(m)).sum();
    let transitions: u64 = l.transitions.iter().map(|&(_, _, n)| u64::from(n)).sum();
    lines.extend([
        format!(
            "Season {}: {} chunks changed in {ms} ms",
            done.season, done.changed_chunks
        ),
        format!(
            "Water: rain {}, vented {}, lost {}",
            l.rain_in, l.steam_vent_out, l.water_saturation_loss
        ),
        format!("Substances: {}", nets(&substance_nets(l), eco)),
        format!("Gases: {}", nets(&gas_nets(l), eco)),
        format!("Plants: {} born, {} died", l.plants_born, l.plants_died),
        format!(
            "Transitions: {transitions}  Weathered: {}  Ignitions: {}",
            l.weathered_cells, l.ignitions
        ),
        format!(
            "Edits before it: water {:+}, substances {}, gases {}",
            e.water_edit_in - e.water_edit_out - e.water_saturation_loss,
            nets(&substance_nets(e), eco),
            nets(&gas_nets(e), eco)
        ),
    ]);
    lines
}

/// The nonzero net change of each substance under `l`, in layer order.
fn substance_nets(l: &SeasonLedger) -> Vec<(LayerId, i64)> {
    (0..N_SUBSTANCES)
        .map(|x| {
            let net = l.weather_in[x] + l.litter_in[x] + l.edit_in[x] + l.react_delta[x]
                - l.edit_out[x]
                - l.uptake_sink[x]
                - l.harvest_out[x]
                - l.saturation_loss[x];
            (LayerId::Substance(x as u8), net)
        })
        .filter(|&(_, net)| net != 0)
        .collect()
}

/// The nonzero net change of each gas under `l`'s per-gas terms, in layer
/// order.
fn gas_nets(l: &SeasonLedger) -> Vec<(LayerId, i64)> {
    (0..N_GASES)
        .map(|g| {
            let net = l.emit_in[g] + l.react_in[g] + l.gas_edit_in[g]
                - l.vent_out[g]
                - l.react_out[g]
                - l.gas_edit_out[g]
                - l.gas_saturation_loss[g];
            (LayerId::Gas(g as u8), net)
        })
        .filter(|&(_, net)| net != 0)
        .collect()
}

/// `name +n, name -n`, or `none`.
fn nets(list: &[(LayerId, i64)], eco: Option<&ClientEco>) -> String {
    if list.is_empty() {
        return "none".to_string();
    }
    list.iter()
        .map(|&(id, net)| format!("{} {net:+}", name_of(id, eco)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The name of layer `id`: from the tables, else its index.
fn name_of(id: LayerId, eco: Option<&ClientEco>) -> String {
    eco.map_or_else(|| unnamed(id), |e| e.layer_name(id))
}

/// The editor's targeted-cell readout: the cell the brush would hit, while
/// the editor is open. Sampled every frame by
/// [`EditorPlugin`](crate::editor::EditorPlugin); empty while the editor is
/// closed or nothing is in reach.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct EditorReadout {
    pub readout: Option<CellReadout>,
    /// [`readout_lines`] of `readout`.
    pub lines: Vec<String>,
}

/// The readout of `cell`, if its chunk is resident.
pub fn cell_readout(world: &World, cell: CellPos, eco: Option<&ClientEco>) -> Option<CellReadout> {
    let material = world.get_cell(cell)?;
    let (pos, local) = cell.split();
    let layers = &world.chunk(pos)?.layers;
    let nonzero = |ids: &mut dyn Iterator<Item = LayerId>| -> Vec<(LayerId, u8)> {
        ids.map(|id| (id, layers.get_u8(id, local)))
            .filter(|&(_, v)| v > 0)
            .collect()
    };
    let substances = nonzero(&mut (0..N_SUBSTANCES as u8).map(LayerId::Substance));
    let gases = nonzero(&mut (0..N_GASES as u8).map(LayerId::Gas));
    Some(CellReadout {
        cell,
        material,
        temperature: layers.get_i16(LayerId::Temperature, local),
        moisture: layers.get_u8(LayerId::Moisture, local),
        regolith: layers.get_u8(LayerId::Regolith, local),
        light: layers.get_u8(LayerId::Light, local),
        ph: eco.map(|e| e.ph.at(world, cell)),
        substances,
        gases,
        spoil_player: eco.map_or_else(Vec::new, |e| e.player_tox.exceeded(world, cell)),
        spoil_plant: eco.map_or_else(Vec::new, |e| e.plant_tox.exceeded(world, cell)),
        plant: world.plant(cell).map(|p| PlantReadout {
            species: p.species,
            growth: layers.get_u8(LayerId::Growth, local),
            age_seasons: p.age_seasons,
            stress: p.stress,
            held: (0..N_SUBSTANCES)
                .map(|x| (LayerId::Substance(x as u8), p.held[x]))
                .filter(|&(_, v)| v > 0)
                .collect(),
        }),
    })
}

/// The readout as the editor panel shows it, one line each: cell and
/// material, temperature, water and rock, pH, substances, gases, a
/// spoil line per profile that the cell exceeds, and for a plant its
/// species, stage (growth, age, stress) and held substances.
pub fn readout_lines(
    r: &CellReadout,
    materials: Option<&MaterialTable>,
    eco: Option<&ClientEco>,
) -> Vec<String> {
    let name = |id: LayerId| name_of(id, eco);
    let amounts = |list: &[(LayerId, u8)]| {
        if list.is_empty() {
            "none".to_string()
        } else {
            list.iter()
                .map(|&(id, v)| format!("{} {v}", name(id)))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let keys = |list: &[LayerId]| {
        list.iter()
            .map(|&id| name(id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let material = materials
        .and_then(|m| m.get(r.material))
        .map_or_else(|| format!("#{}", r.material.0), |d| d.key.clone());
    let p = r.cell.0;
    let mut lines = vec![
        format!("Cell ({}, {}, {}): {material}", p.x, p.y, p.z),
        format!("Temperature: {}", format_temperature(r.temperature)),
        format!(
            "Moisture: {}  Regolith: {}  Light: {}",
            r.moisture, r.regolith, r.light
        ),
        format!(
            "pH: {}",
            r.ph.map_or_else(|| "n/a".to_string(), |ph| ph.to_string())
        ),
        format!("Substances: {}", amounts(&r.substances)),
        format!("Gases: {}", amounts(&r.gases)),
    ];
    if !r.spoil_player.is_empty() {
        lines.push(format!("Spoil (player): {}", keys(&r.spoil_player)));
    }
    if !r.spoil_plant.is_empty() {
        lines.push(format!("Spoil (plants): {}", keys(&r.spoil_plant)));
    }
    if let Some(plant) = &r.plant {
        let species = eco.map_or_else(
            || format!("#{}", plant.species),
            |e| e.species_name(plant.species),
        );
        let mature = if plant.growth == u8::MAX {
            " (mature)"
        } else {
            ""
        };
        lines.push(format!(
            "Plant: {species} · growth {}{mature} · age {} · stress {}",
            plant.growth, plant.age_seasons, plant.stress
        ));
        lines.push(format!("Held: {}", amounts(&plant.held)));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, ChunkPos};
    use rmc_world::query::ToxProfile;

    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    fn eco() -> ClientEco {
        let mut player = [None; N_SUBSTANCES];
        player[0] = Some(80);
        let mut plant = [None; N_SUBSTANCES];
        plant[0] = Some(40);
        plant[1] = Some(64);
        ClientEco {
            substances: SubstanceLayers::new(["cu", "acid", "lime"]).unwrap(),
            gases: vec!["steam".into(), "firedamp".into()],
            ph: PhRule {
                acid: Some(LayerId::Substance(1)),
                lime: Some(LayerId::Substance(2)),
                ph_shift: 2,
            },
            player_tox: ToxProfile::new(player),
            plant_tox: ToxProfile::new(plant),
            tint: Vec::new(),
            gas_tint: Vec::new(),
            flammable: Vec::new(),
            plants: vec!["moss".into(), "fireweed".into()],
        }
    }

    #[test]
    fn readout_lists_layers_ph_and_spoil() {
        let materials = Arc::new(MaterialTable::load(MATERIALS_PATH).unwrap());
        let stone = materials.id("stone").unwrap();
        let bounds = CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31));
        let mut world = World::new(materials.clone(), bounds);
        let cell = CellPos::new(3, 4, 5);
        let (pos, local) = cell.split();
        let mut chunk = Chunk::uniform(stone);
        chunk.layers.set_i16(LayerId::Temperature, local, -15);
        chunk.layers.set_u8(LayerId::Moisture, local, 20);
        chunk.layers.set_u8(LayerId::Regolith, local, 7);
        chunk.layers.set_u8(LayerId::Light, local, 3);
        chunk.layers.set_u8(LayerId::Substance(0), local, 50);
        chunk.layers.set_u8(LayerId::Substance(1), local, 70);
        chunk.layers.set_u8(LayerId::Gas(1), local, 9);
        world.insert_chunk(pos, chunk).unwrap();
        assert_eq!(pos, ChunkPos::new(0, 0, 0));

        let r = cell_readout(&world, cell, Some(&eco())).unwrap();
        assert_eq!(r.material, stone);
        assert_eq!(
            (r.temperature, r.moisture, r.regolith, r.light),
            (-15, 20, 7, 3)
        );
        assert_eq!(r.ph, Some(0), "7 + (0 - 70) / 4 clamps to 0");
        let (cu, acid) = (LayerId::Substance(0), LayerId::Substance(1));
        assert_eq!(r.substances, vec![(cu, 50), (acid, 70)]);
        assert_eq!(r.gases, vec![(LayerId::Gas(1), 9)]);
        assert_eq!(r.spoil_player, vec![], "cu 50 is under the player's 80");
        assert_eq!(r.spoil_plant, vec![cu, acid]);
        assert_eq!(
            readout_lines(&r, Some(&materials), Some(&eco())),
            vec![
                "Cell (3, 4, 5): stone",
                "Temperature: -1.5 C",
                "Moisture: 20  Regolith: 7  Light: 3",
                "pH: 0",
                "Substances: cu 50, acid 70",
                "Gases: firedamp 9",
                "Spoil (plants): cu, acid",
            ]
        );

        // Without the tables: indices for names, no pH, no spoil.
        let bare = cell_readout(&world, cell, None).unwrap();
        assert_eq!((bare.ph, bare.spoil_plant.len()), (None, 0));
        let lines = readout_lines(&bare, None, None);
        assert_eq!(lines[0], format!("Cell (3, 4, 5): #{}", stone.0));
        assert_eq!(lines[3], "pH: n/a");
        assert_eq!(lines[4], "Substances: #0 50, #1 70");
        assert_eq!(lines.len(), 6);

        // A clean cell reads none; a cell outside resident chunks has no
        // readout.
        let clean = cell_readout(&world, CellPos::new(0, 0, 0), Some(&eco())).unwrap();
        let lines = readout_lines(&clean, Some(&materials), Some(&eco()));
        assert_eq!(&lines[3..], ["pH: 7", "Substances: none", "Gases: none"]);
        assert_eq!(
            cell_readout(&world, CellPos::new(40, 0, 0), Some(&eco())),
            None
        );
    }
}
