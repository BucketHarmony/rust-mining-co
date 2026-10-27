//! Client and server messages (SAD §8.4, SAD-P2 §9).

use std::collections::BTreeMap;

use glam::Vec3;
use rmc_world::chunk::{Chunk, ChunkStorage};
use rmc_world::coords::{CellAabb, ChunkPos};
use rmc_world::edit::{EditCommand, EditResult, PlayerId};
use rmc_world::items::{Bag, SlotRef};
use rmc_world::layers::{ChunkLayers, PlantState};
use rmc_world::ledger::SeasonLedger;
use rmc_world::materials::MaterialId;
use serde::{Deserialize, Serialize};

/// Sent in `Hello`; the host rejects a client whose version differs.
/// Version 2 is Phase 2: snapshots carry layers and plants, and seasons.
pub const PROTOCOL_VERSION: u32 = 2;

/// Width and depth of a player's box, in meters. The box's bottom face is
/// centered on `PlayerState::pos`, the feet. Narrower than a cell, so the
/// player fits a one-cell hole or shaft: an owner change (F094) to SAD
/// §11.4's 0.6 m.
pub const PLAYER_WIDTH_M: f32 = 0.45;
/// Height of a player's box, in meters (SAD §11.4).
pub const PLAYER_HEIGHT_M: f32 = 1.7;

/// Client → host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First message on a connection.
    Hello {
        protocol: u32,
        material_hash: u64,
    },
    Edit(EditCommand),
    /// Where the player is: `pos` is the feet (see [`PLAYER_WIDTH_M`]).
    PlayerState {
        pos: Vec3,
        yaw: f32,
        pitch: f32,
    },
    /// The replica asks for a snapshot after a revision gap.
    RequestChunk(ChunkPos),
    /// Editor "Save" button.
    SaveRequest,
    /// Editor "Season" button: run one season tick (SAD-P2 §9).
    AdvanceSeason,
    /// Move `count` (`None`: all) of the stack in one bag slot to another
    /// (`docs/ITEMS.md`). Hosts without an item table ignore it, so the
    /// version stays 2.
    MoveItem {
        from: SlotRef,
        to: SlotRef,
        count: Option<u32>,
    },
}

/// What a season tick did (SAD-P2 §9): its ledger, the ledger of the edits
/// made since the previous season, and timing and activity per sub-step.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeasonReport {
    pub ledger: SeasonLedger,
    pub between_seasons: SeasonLedger,
    pub substep_ms: Vec<u32>,
    pub active_cells_per_sweep: Vec<u32>,
    pub active_chunks_per_jacobi: Vec<u32>,
}

/// The game calendar's hour (`docs/CALENDAR.md`): where a calendar season
/// stands after a sub-step, and the weather that sub-step ran with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameClock {
    pub season: u32,
    /// Day of the season, from 0, of `days`.
    pub day: u8,
    pub days: u8,
    /// Hour of the day, from 0, of `hours`: the sub-step just run.
    pub hour: u8,
    pub hours: u8,
    /// The hour's air temperature, 0.1 °C.
    pub air_temp: i16,
    /// The hour's rain per column before the pattern; 0 when dry.
    pub rain: u8,
    /// The calendar's wettest hour (the largest season `rain`), so the
    /// client can tell light rain from heavy.
    pub peak_rain: u8,
    /// Whether the hour's precipitation is snow.
    pub snow: bool,
    /// Sunrise and sunset of the season, in hours.
    pub daylight: (u8, u8),
}

/// Host → client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome {
        player: PlayerId,
        seed: u64,
        bounds: CellAabb,
        gen_params_hash: u64,
    },
    /// `Hello` mismatch.
    Rejected {
        reason: String,
    },
    /// The join encoding of a chunk (v2): cells, layers and plants.
    ChunkSnapshot {
        pos: ChunkPos,
        revision: u64,
        eco_revision: u64,
        storage: ChunkStorage,
        layers: ChunkLayers,
        plants: BTreeMap<u16, PlantState>,
    },
    /// A chunk whose only change in a season was ecological.
    LayerSnapshot {
        pos: ChunkPos,
        eco_revision: u64,
        layers: ChunkLayers,
        plants: BTreeMap<u16, PlantState>,
    },
    Applied(EditResult),
    Inventory {
        items: Vec<(MaterialId, u32)>,
    },
    /// Unused in Phase 1; the shape is reserved for co-op.
    PlayerStates(Vec<(PlayerId, Vec3, f32, f32)>),
    Saved {
        ok: bool,
        path: String,
    },
    /// A season tick began; edits are paused until `SeasonDone`.
    SeasonStarted {
        season: u32,
    },
    /// A season tick finished, after its snapshots.
    SeasonDone {
        season: u32,
        report: Box<SeasonReport>,
        changed_chunks: u32,
    },
    /// A sub-step of a calendar season ran (after its snapshots): the
    /// clock and the hour's weather. Hosts without a calendar never send
    /// it, so the version stays 2.
    Clock(GameClock),
    /// The player's bag, after it changed or a `MoveItem` (`docs/ITEMS.md`).
    /// Hosts without an item table never send it, so the version stays 2.
    Bag(Box<Bag>),
}

impl ServerMsg {
    /// The v2 join encoding of a resident chunk: cells, layers and plants
    /// with both revisions.
    pub fn chunk_snapshot(pos: ChunkPos, chunk: &Chunk) -> Self {
        ServerMsg::ChunkSnapshot {
            pos,
            revision: chunk.revision,
            eco_revision: chunk.eco_revision,
            storage: chunk.storage.clone(),
            layers: chunk.layers.clone(),
            plants: chunk.plants.clone(),
        }
    }
}
