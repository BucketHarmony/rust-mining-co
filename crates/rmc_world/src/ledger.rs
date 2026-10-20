//! The season ledger (SAD-P2 §7): every source and sink of water, each
//! substance, each gas and magma, as actual amounts moved.
//!
//! Plain data. `World::apply` writes an edit's displacement terms into the
//! ledger on its `EditResult`; `World::eco_set` writes a pass's terms into
//! the season's ledger. `rmc_eco` re-exports this type and owns the
//! invariant checker.

use serde::{Deserialize, Serialize};

use crate::layers::{N_GASES, N_SUBSTANCES};
use crate::materials::MaterialId;

/// Typed sources and sinks for one season (or one edit). Gas arrays are
/// indexed by gas index, substance arrays by substance index.
///
/// Beyond the SAD-P2 §7 sketch it carries `water_saturation_loss` (water a
/// pass displaced with nowhere to go) and `gas_edit_in` (gas added by
/// `EditOp::SetLayer`), so that every displacement case has a term.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeasonLedger {
    // Water units.
    pub rain_in: i64,
    pub steam_vent_out: i64,
    pub water_edit_in: i64,
    pub water_edit_out: i64,
    pub water_saturation_loss: i64,
    /// Informational: water to steam within the water total.
    pub evap_to_steam: i64,
    pub boil_to_steam: i64,
    pub condense_from_steam: i64,
    // Substances.
    pub weather_in: [i64; N_SUBSTANCES],
    pub litter_in: [i64; N_SUBSTANCES],
    pub uptake_sink: [i64; N_SUBSTANCES],
    pub harvest_out: [i64; N_SUBSTANCES],
    pub edit_in: [i64; N_SUBSTANCES],
    pub edit_out: [i64; N_SUBSTANCES],
    pub react_delta: [i64; N_SUBSTANCES],
    pub saturation_loss: [i64; N_SUBSTANCES],
    // Gases. Steam's entries mirror the water entries above.
    pub emit_in: [i64; N_GASES],
    pub vent_out: [i64; N_GASES],
    pub react_out: [i64; N_GASES],
    pub react_in: [i64; N_GASES],
    pub gas_edit_in: [i64; N_GASES],
    pub gas_edit_out: [i64; N_GASES],
    pub gas_saturation_loss: [i64; N_GASES],
    // Magma, in cells.
    /// Cells turned to basalt or obsidian.
    pub magma_set: i64,
    /// Stone melted.
    pub magma_melt_in: i64,
    /// Net magma cells added by edits.
    pub magma_edit: i64,
    // Events.
    pub transitions: Vec<(MaterialId, MaterialId, u32)>,
    pub weathered_cells: u32,
    pub plants_born: u32,
    pub plants_died: u32,
    pub ignitions: u32,
}
